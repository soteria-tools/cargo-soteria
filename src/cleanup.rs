use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

fn main() {
    println!("Soteria Cleanup Utility");
    println!("======================\n");

    let home = match env::var("HOME") {
        Ok(h) => h,
        Err(_) => {
            eprintln!("Error: HOME environment variable not set");
            std::process::exit(1);
        }
    };

    let soteria_dir = PathBuf::from(home).join(".soteria");

    if !soteria_dir.exists() {
        println!("✓ No ~/.soteria directory found. Nothing to clean up.");
        return;
    }

    // Calculate directory size
    let size = match get_dir_size(&soteria_dir) {
        Ok(s) => format_size(s),
        Err(_) => "unknown".to_string(),
    };

    println!("Found ~/.soteria directory");
    println!("Location: {}", soteria_dir.display());
    println!("Size: {}", size);
    println!();

    // List versions
    if let Ok(entries) = fs::read_dir(&soteria_dir) {
        let versions: Vec<_> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .collect();

        if !versions.is_empty() {
            println!("Installed versions:");
            for entry in &versions {
                let version_name = entry.file_name();
                println!("  - {}", version_name.to_string_lossy());
            }
            println!();
        }
    }

    print!("Do you want to remove the ~/.soteria directory? [y/N] ");
    io::stdout().flush().unwrap();

    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap();

    if input.trim().eq_ignore_ascii_case("y") {
        print!("Removing {}... ", soteria_dir.display());
        io::stdout().flush().unwrap();

        match fs::remove_dir_all(&soteria_dir) {
            Ok(_) => {
                println!("✓ Done!");
                println!("\nCleanup complete. Freed approximately {}", size);
            }
            Err(e) => {
                println!("✗ Failed");
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
    } else {
        println!("Cleanup cancelled.");
    }
}

fn get_dir_size(path: &PathBuf) -> io::Result<u64> {
    let mut total = 0;

    if path.is_dir() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let metadata = entry.metadata()?;

            if metadata.is_dir() {
                total += get_dir_size(&entry.path())?;
            } else {
                total += metadata.len();
            }
        }
    }

    Ok(total)
}

fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} bytes", bytes)
    }
}
