use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set"));
    let icon_64 = build_dib_icon(64);
    let icon_32 = build_dib_icon(32);
    let grp_icon = build_group_icon_dir(&[(64, icon_64.len() as u32, 1), (32, icon_32.len() as u32, 2)]);
    let manifest = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0" xmlns:asmv3="urn:schemas-microsoft-com:asm.v3">
  <assemblyIdentity type="win32" name="Davin.CopyScreen" version="0.1.0.0" processorArchitecture="amd64"/>
  <description>CopyScreen - Open Source Screen Text Extractor</description>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
  <asmv3:application>
    <asmv3:windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
    </asmv3:windowsSettings>
  </asmv3:application>
</assembly>"#;

    let coff_bytes = build_rsrc_coff_x64(&icon_64, &icon_32, &grp_icon, manifest);
    let obj_path = out_dir.join("copyscreen_resources.o");
    fs::write(&obj_path, coff_bytes).expect("Failed to write COFF resource object");

    println!("cargo:rustc-link-arg-bins={}", obj_path.display());
}

/// Builds a 32-bit BGRA DIB + 1-bit AND mask suitable for `RT_ICON` resource payload.
fn build_dib_icon(size: usize) -> Vec<u8> {
    let xor_size = size * size * 4;
    let mask_row_bytes = ((size + 31) / 32) * 4;
    let and_size = mask_row_bytes * size;

    let mut out = Vec::with_capacity(40 + xor_size + and_size);
    // BITMAPINFOHEADER (40 bytes) — height is doubled for icon DIBs (XOR + AND mask)
    push_u32(&mut out, 40);
    push_i32(&mut out, size as i32);
    push_i32(&mut out, (size * 2) as i32);
    push_u16(&mut out, 1); // biPlanes
    push_u16(&mut out, 32); // biBitCount
    push_u32(&mut out, 0); // BI_RGB
    push_u32(&mut out, (xor_size + and_size) as u32);
    push_i32(&mut out, 0);
    push_i32(&mut out, 0);
    push_u32(&mut out, 0);
    push_u32(&mut out, 0);

    let center = (size as f32 - 1.0) * 0.5;
    let scale = size as f32 / 32.0;
    let half_box = 13.8 * scale;
    let corner_r = 6.5 * scale;

    // Note: RT_ICON DIB rows are stored bottom-up!
    for y_inv in 0..size {
        let y = size - 1 - y_inv;
        for x in 0..size {
            let fx = x as f32;
            let fy = y as f32;

            let dx = (fx - center).abs() - (half_box - corner_r);
            let dy = (fy - center).abs() - (half_box - corner_r);
            let outside_dist = (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt()
                + dx.max(dy).min(0.0)
                - corner_r;

            if outside_dist > 0.8 {
                out.extend_from_slice(&[0, 0, 0, 0]);
                continue;
            }

            let alpha = (0.8 - outside_dist).clamp(0.0, 1.0);
            let nx = (x as f32 + 0.5) / scale;
            let ny = (y as f32 + 0.5) / scale;

            let (r, g, b) = sample_option2_logo_rgb(nx, ny, outside_dist / scale);

            let a_u8 = (alpha * 255.0) as u8;
            out.push(b);
            out.push(g);
            out.push(r);
            out.push(a_u8);
        }
    }

    // 1-bit AND mask (all zeros since 32-bit alpha channel controls transparency)
    out.resize(out.len() + and_size, 0);
    out
}

fn sdf_rrect(px: f32, py: f32, x1: f32, y1: f32, x2: f32, y2: f32, r: f32) -> f32 {
    let cx = (x1 + x2) * 0.5;
    let cy = (y1 + y2) * 0.5;
    let hx = (x2 - x1) * 0.5 - r;
    let hy = (y2 - y1) * 0.5 - r;
    let dx = (px - cx).abs() - hx;
    let dy = (py - cy).abs() - hy;
    (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt() + dx.max(dy).min(0.0) - r
}

/// Renders Option 2: "Overlapping Screen Copy Sheets" at normalized 32x32 coordinates.
fn sample_option2_logo_rgb(nx: f32, ny: f32, outer_sd: f32) -> (u8, u8, u8) {
    // 1. Base #111111 squircle with subtle #303030 rim
    let mut r = 17.0_f32;
    let mut g = 17.0_f32;
    let mut b = 17.0_f32;
    if outer_sd > -1.1 {
        r = 48.0;
        g = 48.0;
        b = 48.0;
    }

    // 2. Back Screen Sheet (x: 5.8..21.0, y: 5.6..20.8, radius 2.2)
    let back_sd = sdf_rrect(nx, ny, 5.8, 5.6, 21.0, 20.8, 2.2);
    if back_sd < 0.5 {
        let cov = (0.5 - back_sd).clamp(0.0, 1.0);
        let is_border = back_sd > -0.9;
        let (br, bg, bb) = if is_border {
            (82.0, 82.0, 82.0)
        } else {
            (34.0, 34.0, 34.0)
        };
        r = r * (1.0 - cov) + br * cov;
        g = g * (1.0 - cov) + bg * cov;
        b = b * (1.0 - cov) + bb * cov;
    }

    // 3. Soft Shadow behind Front Sheet
    let shadow_sd = sdf_rrect(nx, ny, 9.4, 9.4, 26.2, 26.2, 2.6);
    if shadow_sd < 1.2 {
        let scov = ((1.2 - shadow_sd) / 2.4).clamp(0.0, 0.65);
        r *= 1.0 - scov;
        g *= 1.0 - scov;
        b *= 1.0 - scov;
    }

    // 4. Front Copied Sheet (#F2F2F2, x: 10.2..26.0, y: 10.0..25.8, radius 2.3)
    let front_sd = sdf_rrect(nx, ny, 10.2, 10.0, 26.0, 25.8, 2.3);
    if front_sd < 0.5 {
        let cov = (0.5 - front_sd).clamp(0.0, 1.0);
        let mut fr = 242.0_f32;
        let mut fg = 242.0_f32;
        let mut fb = 242.0_f32;

        // Orange Selection Highlight Block (#F97316)
        let orange_sd = sdf_rrect(nx, ny, 12.2, 12.5, 24.0, 16.3, 0.85);
        if orange_sd < 0.45 {
            let ocov = (0.45 - orange_sd).clamp(0.0, 1.0);
            fr = fr * (1.0 - ocov) + 249.0 * ocov;
            fg = fg * (1.0 - ocov) + 115.0 * ocov;
            fb = fb * (1.0 - ocov) + 22.0 * ocov;

            // Inner white selected text bar
            let wbar_sd = sdf_rrect(nx, ny, 13.2, 13.8, 21.5, 15.0, 0.4);
            if wbar_sd < 0.4 {
                let wcov = (0.4 - wbar_sd).clamp(0.0, 1.0);
                fr = fr * (1.0 - wcov) + 255.0 * wcov;
                fg = fg * (1.0 - wcov) + 255.0 * wcov;
                fb = fb * (1.0 - wcov) + 255.0 * wcov;
            }
        }

        // Dark ink text row 1
        let bar1_sd = sdf_rrect(nx, ny, 12.2, 18.0, 23.0, 19.5, 0.45);
        if bar1_sd < 0.45 {
            let bcov = (0.45 - bar1_sd).clamp(0.0, 1.0);
            fr = fr * (1.0 - bcov) + 22.0 * bcov;
            fg = fg * (1.0 - bcov) + 22.0 * bcov;
            fb = fb * (1.0 - bcov) + 22.0 * bcov;
        }

        // Muted ink text row 2
        let bar2_sd = sdf_rrect(nx, ny, 12.2, 20.8, 19.4, 22.3, 0.45);
        if bar2_sd < 0.45 {
            let bcov = (0.45 - bar2_sd).clamp(0.0, 1.0);
            fr = fr * (1.0 - bcov) + 115.0 * bcov;
            fg = fg * (1.0 - bcov) + 115.0 * bcov;
            fb = fb * (1.0 - bcov) + 115.0 * bcov;
        }

        r = r * (1.0 - cov) + fr * cov;
        g = g * (1.0 - cov) + fg * cov;
        b = b * (1.0 - cov) + fb * cov;
    }

    (r as u8, g as u8, b as u8)
}

/// Builds `GRPICONDIR` structure for `RT_GROUP_ICON` (Type 14).
fn build_group_icon_dir(entries: &[(u8, u32, u16)]) -> Vec<u8> {
    let mut out = Vec::new();
    push_u16(&mut out, 0); // idReserved
    push_u16(&mut out, 1); // idType = 1 (ICON)
    push_u16(&mut out, entries.len() as u16);

    for &(dim, bytes_in_res, id) in entries {
        out.push(dim); // bWidth
        out.push(dim); // bHeight
        out.push(0); // bColorCount
        out.push(0); // bReserved
        push_u16(&mut out, 1); // wPlanes
        push_u16(&mut out, 32); // wBitCount
        push_u32(&mut out, bytes_in_res);
        push_u16(&mut out, id); // nID
    }
    out
}

/// Constructs a valid x86_64 COFF `.o` object containing `.rsrc` with:
/// - Type 3 (`RT_ICON`): IDs 1 and 2
/// - Type 14 (`RT_GROUP_ICON`): ID 1
/// - Type 24 (`RT_MANIFEST`): ID 1
fn build_rsrc_coff_x64(
    icon_64: &[u8],
    icon_32: &[u8],
    grp_icon: &[u8],
    manifest: &[u8],
) -> Vec<u8> {
    // Resource Directory Tree layout inside `.rsrc`:
    // Level 1 (Root, offset 0): 16B header + 3 entries (24B) = 40 bytes (0..40)
    //   Entry 0: Type 3  (RT_ICON)       -> Subdir at offset 40
    //   Entry 1: Type 14 (RT_GROUP_ICON) -> Subdir at offset 72
    //   Entry 2: Type 24 (RT_MANIFEST)   -> Subdir at offset 96
    // Level 2 (IDs, offsets 40..120):
    //   Subdir @ 40 (RT_ICON):       16B + 2 entries (16B) = 32B (40..72)
    //     ID 1 -> Subdir @ 120
    //     ID 2 -> Subdir @ 144
    //   Subdir @ 72 (RT_GROUP_ICON): 16B + 1 entry   (8B)  = 24B (72..96)
    //     ID 1 -> Subdir @ 168
    //   Subdir @ 96 (RT_MANIFEST):   16B + 1 entry   (8B)  = 24B (96..120)
    //     ID 1 -> Subdir @ 192
    // Level 3 (Language 0x0409, offsets 120..216):
    //   Subdir @ 120 (Icon 1 Lang):   16B + 1 entry (8B) = 24B -> DataEntry @ 216
    //   Subdir @ 144 (Icon 2 Lang):   16B + 1 entry (8B) = 24B -> DataEntry @ 232
    //   Subdir @ 168 (GrpIcon Lang):  16B + 1 entry (8B) = 24B -> DataEntry @ 248
    //   Subdir @ 192 (Manifest Lang): 16B + 1 entry (8B) = 24B -> DataEntry @ 264
    // Data Entries (4 * 16B = 64B, offsets 216..280):
    //   DataEntry 0 @ 216 (icon_64)
    //   DataEntry 1 @ 232 (icon_32)
    //   DataEntry 2 @ 248 (grp_icon)
    //   DataEntry 3 @ 264 (manifest)
    let mut rsrc = Vec::new();

    // Level 1: Root (0..40)
    push_dir_header(&mut rsrc, 0, 3);
    push_dir_entry(&mut rsrc, 3, true, 40);
    push_dir_entry(&mut rsrc, 14, true, 72);
    push_dir_entry(&mut rsrc, 24, true, 96);

    // Level 2: Type 3 (40..72)
    push_dir_header(&mut rsrc, 0, 2);
    push_dir_entry(&mut rsrc, 1, true, 120);
    push_dir_entry(&mut rsrc, 2, true, 144);

    // Level 2: Type 14 (72..96)
    push_dir_header(&mut rsrc, 0, 1);
    push_dir_entry(&mut rsrc, 1, true, 168);

    // Level 2: Type 24 (96..120)
    push_dir_header(&mut rsrc, 0, 1);
    push_dir_entry(&mut rsrc, 1, true, 192);

    // Level 3: Languages (120..216)
    push_dir_header(&mut rsrc, 0, 1);
    push_dir_entry(&mut rsrc, 0x0409, false, 216);

    push_dir_header(&mut rsrc, 0, 1);
    push_dir_entry(&mut rsrc, 0x0409, false, 232);

    push_dir_header(&mut rsrc, 0, 1);
    push_dir_entry(&mut rsrc, 0x0409, false, 248);

    push_dir_header(&mut rsrc, 0, 1);
    push_dir_entry(&mut rsrc, 0x0409, false, 264);

    assert_eq!(rsrc.len(), 216);

    let payloads: [&[u8]; 4] = [icon_64, icon_32, grp_icon, manifest];
    let mut payload_offsets = [0u32; 4];
    let mut cur_offset = 280u32;
    for (i, p) in payloads.iter().enumerate() {
        payload_offsets[i] = cur_offset;
        let aligned_len = ((p.len() + 7) & !7) as u32;
        cur_offset += aligned_len;
    }

    // 4 IMAGE_RESOURCE_DATA_ENTRY structs (216..280)
    let reloc_addrs = [216u32, 232u32, 248u32, 264u32];
    for (i, p) in payloads.iter().enumerate() {
        push_u32(&mut rsrc, payload_offsets[i]); // OffsetToData (relocated via ADDR32NB)
        push_u32(&mut rsrc, p.len() as u32); // Size
        push_u32(&mut rsrc, 0); // CodePage
        push_u32(&mut rsrc, 0); // Reserved
    }
    assert_eq!(rsrc.len(), 280);

    for p in &payloads {
        rsrc.extend_from_slice(p);
        while rsrc.len() % 8 != 0 {
            rsrc.push(0);
        }
    }

    // Build COFF file:
    // Header (20B) + 1 Section Header (40B) = 60B
    // Raw section data: rsrc.len() bytes at offset 60
    // Relocations: 4 * 10B = 40B at offset 60 + rsrc.len()
    // Symbol table: 1 symbol (18B) + 4B string table = 22B at offset 60 + rsrc.len() + 40
    let raw_data_ptr = 60u32;
    let reloc_ptr = raw_data_ptr + rsrc.len() as u32;
    let symtab_ptr = reloc_ptr + 40;

    let mut coff = Vec::new();
    // IMAGE_FILE_HEADER (20B)
    push_u16(&mut coff, 0x8664); // IMAGE_FILE_MACHINE_AMD64
    push_u16(&mut coff, 1); // NumberOfSections
    push_u32(&mut coff, 0); // TimeDateStamp
    push_u32(&mut coff, symtab_ptr); // PointerToSymbolTable
    push_u32(&mut coff, 1); // NumberOfSymbols
    push_u16(&mut coff, 0); // SizeOfOptionalHeader
    push_u16(&mut coff, 0); // Characteristics

    // IMAGE_SECTION_HEADER (40B)
    coff.extend_from_slice(b".rsrc\0\0\0");
    push_u32(&mut coff, 0); // VirtualSize
    push_u32(&mut coff, 0); // VirtualAddress
    push_u32(&mut coff, rsrc.len() as u32); // SizeOfRawData
    push_u32(&mut coff, raw_data_ptr); // PointerToRawData
    push_u32(&mut coff, reloc_ptr); // PointerToRelocations
    push_u32(&mut coff, 0); // PointerToLinenumbers
    push_u16(&mut coff, 4); // NumberOfRelocations
    push_u16(&mut coff, 0); // NumberOfLinenumbers
    push_u32(&mut coff, 0x4000_0040); // IMAGE_SCN_CNT_INITIALIZED_DATA | IMAGE_SCN_MEM_READ

    // Section data
    coff.extend_from_slice(&rsrc);

    // 4 Relocations (each 10 bytes)
    for &addr in &reloc_addrs {
        push_u32(&mut coff, addr); // VirtualAddress
        push_u32(&mut coff, 0); // SymbolTableIndex (0 = .rsrc symbol)
        push_u16(&mut coff, 0x0003); // IMAGE_REL_AMD64_ADDR32NB
    }

    // 1 Symbol Table Entry (18 bytes) for `.rsrc`
    coff.extend_from_slice(b".rsrc\0\0\0");
    push_u32(&mut coff, 0); // Value
    push_i16(&mut coff, 1); // SectionNumber = 1
    push_u16(&mut coff, 0); // Type
    coff.push(3); // StorageClass = IMAGE_SYM_CLASS_STATIC
    coff.push(0); // NumberOfAuxSymbols

    // String table (4 bytes = size 4)
    push_u32(&mut coff, 4);

    coff
}

fn push_dir_header(buf: &mut Vec<u8>, named_entries: u16, id_entries: u16) {
    push_u32(buf, 0); // Characteristics
    push_u32(buf, 0); // TimeDateStamp
    push_u16(buf, 0); // MajorVersion
    push_u16(buf, 0); // MinorVersion
    push_u16(buf, named_entries);
    push_u16(buf, id_entries);
}

fn push_dir_entry(buf: &mut Vec<u8>, id: u32, is_subdir: bool, offset: u32) {
    push_u32(buf, id);
    let off = if is_subdir {
        offset | 0x8000_0000
    } else {
        offset
    };
    push_u32(buf, off);
}

fn push_u16(buf: &mut Vec<u8>, v: u16) {
    buf.extend_from_slice(&v.to_le_bytes());
}
fn push_i16(buf: &mut Vec<u8>, v: i16) {
    buf.extend_from_slice(&v.to_le_bytes());
}
fn push_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}
fn push_i32(buf: &mut Vec<u8>, v: i32) {
    buf.extend_from_slice(&v.to_le_bytes());
}
