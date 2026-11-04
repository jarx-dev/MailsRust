//! Puts Microsoft's WebView2 loader (needed to show formatted emails with the Edge engine)
//! next to the built program, so it starts on any Windows machine with WebView2 installed,
//! and builds the app's icon into the program file itself, so Windows shows it everywhere
//! (Explorer, taskbar, shortcuts) right away, even before the window opens.

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=assets/WebView2Loader.dll");
    println!("cargo:rerun-if-changed=assets/poczta.ico");
    build_identity();
    if std::env::var("CARGO_CFG_WINDOWS").is_err() {
        return;
    }
    let Ok(out) = std::env::var("OUT_DIR") else { return };
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    // OUT_DIR is <target>/<profile>/build/<package>/out; the program goes into <target>/<profile>.
    let out = PathBuf::from(out);
    if let Some(profile_dir) = out.ancestors().nth(3) {
        let src = manifest.join("assets").join("WebView2Loader.dll");
        let dest = profile_dir.join("WebView2Loader.dll");
        if src.exists() && !dest.exists() {
            let _ = std::fs::copy(&src, &dest);
        }
        let deps = profile_dir.join("deps").join("WebView2Loader.dll");
        if src.exists() && !deps.exists() {
            let _ = std::fs::copy(&src, &deps);
        }
    }
    if let Err(e) = embed_icon(&manifest, &out) {
        println!("cargo:warning=program icon not embedded: {e}");
    }
}

/// The build's identity for the self-updater (src/update.rs). scripts/publish.sh sets both, so
/// the EXE and its latest.json agree; other builds use the git commit and the current time.
fn build_identity() {
    println!("cargo:rerun-if-env-changed=POCZTA_BUILD");
    println!("cargo:rerun-if-env-changed=POCZTA_BUILT_AT");
    let built_at = std::env::var("POCZTA_BUILT_AT")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or_else(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0));
    let published = std::env::var("POCZTA_BUILD").ok().filter(|v| !v.trim().is_empty());
    println!("cargo:rustc-env=POCZTA_PUBLISHED={}", if published.is_some() { "1" } else { "" });
    let build = published.unwrap_or_else(|| {
        let sha = std::process::Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        format!("{}-{built_at}", sha.unwrap_or_else(|| "local".into()))
    });
    println!("cargo:rustc-env=POCZTA_BUILD={build}");
    println!("cargo:rustc-env=POCZTA_BUILT_AT={built_at}");
}

/// Turns assets/poczta.ico into a Windows resource and links it into the program.
fn embed_icon(manifest: &Path, out: &Path) -> Result<(), String> {
    let ico = std::fs::read(manifest.join("assets").join("poczta.ico")).map_err(|e| e.to_string())?;
    if ico.len() < 6 || ico[2] != 1 {
        return Err("not an icon file".into());
    }
    let count = u16::from_le_bytes([ico[4], ico[5]]) as usize;
    let u16_at = |i: usize| u16::from_le_bytes([ico[i], ico[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes([ico[i], ico[i + 1], ico[i + 2], ico[i + 3]]);

    // The compiled resource format: an empty first entry, one entry per picture, then the
    // group that lists them.
    let mut res = Vec::new();
    let entry = |res: &mut Vec<u8>, kind: u16, id: u16, flags: u16, data: &[u8]| {
        res.extend_from_slice(&(data.len() as u32).to_le_bytes());
        res.extend_from_slice(&32u32.to_le_bytes());
        res.extend_from_slice(&[0xFF, 0xFF]);
        res.extend_from_slice(&kind.to_le_bytes());
        res.extend_from_slice(&[0xFF, 0xFF]);
        res.extend_from_slice(&id.to_le_bytes());
        res.extend_from_slice(&0u32.to_le_bytes());
        res.extend_from_slice(&flags.to_le_bytes());
        res.extend_from_slice(&0x0409u16.to_le_bytes());
        res.extend_from_slice(&0u32.to_le_bytes());
        res.extend_from_slice(&0u32.to_le_bytes());
        res.extend_from_slice(data);
        while res.len() % 4 != 0 {
            res.push(0);
        }
    };
    entry(&mut res, 0, 0, 0, &[]);
    let mut group = vec![0u8, 0, 1, 0];
    group.extend_from_slice(&(count as u16).to_le_bytes());
    for i in 0..count {
        let e = 6 + 16 * i;
        let size = u32_at(e + 8) as usize;
        let at = u32_at(e + 12) as usize;
        let data = ico.get(at..at + size).ok_or("damaged icon file")?;
        entry(&mut res, 3, (i + 1) as u16, 0x1010, data);
        group.extend_from_slice(&ico[e..e + 4]);
        group.extend_from_slice(&u16_at(e + 4).to_le_bytes());
        group.extend_from_slice(&u16_at(e + 6).to_le_bytes());
        group.extend_from_slice(&(size as u32).to_le_bytes());
        group.extend_from_slice(&((i + 1) as u16).to_le_bytes());
    }
    entry(&mut res, 14, 1, 0x1030, &group);
    let res_path = out.join("poczta.res");
    std::fs::write(&res_path, &res).map_err(|e| e.to_string())?;
    // the Microsoft linker (also lld-link from cargo-xwin) takes the resource file as it is
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins={}", res_path.display());
        return Ok(());
    }

    // windres turns it into an object file the linker understands
    let windres = manifest.join(".setup").join("binutils").join("windres.exe");
    let tool = if windres.exists() { windres.into_os_string() } else { "windres".into() };
    let obj = out.join("poczta-icon.o");
    let status = std::process::Command::new(tool)
        .args(["-J", "res", "-O", "coff", "-i"])
        .arg(&res_path)
        .arg("-o")
        .arg(&obj)
        .status()
        .map_err(|e| format!("windres: {e}"))?;
    if !status.success() {
        return Err("windres failed".into());
    }
    println!("cargo:rustc-link-arg-bins={}", obj.display());
    Ok(())
}
