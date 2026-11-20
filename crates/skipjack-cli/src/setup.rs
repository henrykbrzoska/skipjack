//! Interactive command line setup.

use std::io::{self, BufRead, Write};
use std::path::Path;

use anyhow::Result;
use skipjack_core::config::{config_path, load_from_data_dir};
use skipjack_core::{build_http_client, discover, pair_with_code, Device};

fn input(prompt: &str) -> String {
    print!("{prompt}");
    let _ = io::stdout().flush();
    let mut line = String::new();
    let _ = io::stdin().lock().read_line(&mut line);
    line.trim().to_string()
}

/// y/n question; empty input returns `default`.
fn yes_no(prompt: &str, default: bool) -> bool {
    let hint = if default { "(Y/n)" } else { "(y/N)" };
    loop {
        match input(&format!("{prompt} {hint} ")).to_lowercase().as_str() {
            "" => return default,
            "y" | "yes" | "t" | "tak" => return true,
            "n" | "no" | "nie" => return false,
            _ => println!("Invalid input. Please enter 'y' or 'n'."),
        }
    }
}

pub async fn run(data_dir: &Path) -> Result<()> {
    println!("Welcome to the Skipjack command line setup");
    let mut cfg = load_from_data_dir(data_dir)?;

    cfg.use_proxy = yes_no("Do you want to use the system-wide proxy?", cfg.use_proxy);
    let client = build_http_client(cfg.use_proxy);

    if cfg.extra.contains_key("atvs")
        && yes_no(
            "Remove the legacy 'atvs' entry (the app won't start with it present)?",
            true,
        )
    {
        cfg.extra.remove("atvs");
    }

    while yes_no(
        &format!("{} device(s) configured. Add more?", cfg.devices.len()),
        cfg.devices.is_empty(),
    ) {
        if yes_no("Search the local network for devices?", true) {
            println!("Searching (up to ~30 s, the YouTube app may open on your TVs)...");
            let mut rx = discover(client.clone(), true);
            let mut found = Vec::new();
            while let Some(d) = rx.recv().await {
                println!("  {}: {} ({:?})", found.len(), d.name, d.source);
                found.push(d);
            }
            if found.is_empty() {
                println!("No devices found.");
            }
            for d in found {
                if cfg.devices.iter().any(|x| x.screen_id == d.screen_id) {
                    continue;
                }
                if yes_no(&format!("Add '{}'?", d.name), true) {
                    cfg.devices.push(Device::new(d.screen_id, d.name));
                }
            }
        } else {
            let code = input("Enter pairing code (Settings → Link with TV code): ");
            println!("Pairing...");
            match pair_with_code(&client, &code).await {
                Ok(Some(p)) => {
                    let name = p.name.unwrap_or_else(|| "YouTube on TV".into());
                    println!("Paired device: {name}");
                    cfg.devices.retain(|d| d.screen_id != p.screen_id);
                    cfg.devices.push(Device::new(p.screen_id, name));
                }
                Ok(None) => println!("Failed to pair device. Try restarting the YouTube app"),
                Err(e) => println!("Failed to pair device: {e:#}"),
            }
        }
    }

    cfg.mute_ads = yes_no("Mute YouTube ads automatically?", cfg.mute_ads);
    cfg.skip_ads = yes_no(
        "Skip YouTube ads automatically (when skippable)?",
        cfg.skip_ads,
    );
    cfg.auto_play = yes_no("Enable autoplay?", cfg.auto_play);

    let name = input(&format!(
        "Name shown on the TV (empty = {}): ",
        cfg.join_name
    ));
    if !name.is_empty() {
        cfg.join_name = name;
    }

    let path = config_path(data_dir);
    cfg.save(&path)?;
    println!("Config saved to {}", path.display());
    Ok(())
}
