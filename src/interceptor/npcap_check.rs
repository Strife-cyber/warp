#![allow(dead_code)]

#[cfg(feature = "capture")]
/// Checks if Npcap/WinPcap is installed on the system
pub fn check_npcap_installed() -> bool {
    // Check if wpcap.dll exists in system directories
    let system_paths = vec![
        r"C:\Windows\System32\wpcap.dll",
        r"C:\Windows\SysWOW64\wpcap.dll",
    ];
    
    for path in system_paths {
        if std::path::Path::new(path).exists() {
            return true;
        }
    }
    
    false
}

#[cfg(feature = "capture")]
#[allow(dead_code)]
/// Downloads Npcap installer and prompts user to install it
pub fn download_and_prompt_npcap_install() -> anyhow::Result<()> {
    use std::io::Read;
    
    println!("Npcap is not installed. Downloading installer...");
    
    let npcap_url = "https://nmap.org/dist/npcap-1.75.exe";
    let temp_dir = std::env::temp_dir();
    let installer_path = temp_dir.join("npcap-installer.exe");
    
    // Download the installer
    let response = reqwest::blocking::get(npcap_url)?;
    let mut file = std::fs::File::create(&installer_path)?;
    let mut response = response.take(10 * 1024 * 1024 /* 10MB limit */);
    std::io::copy(&mut response, &mut file)?;
    
    println!("Downloaded Npcap installer to: {}", installer_path.display());
    println!("\nIMPORTANT: During installation, enable 'WinPcap API-compatible Mode'");
    println!("After installation, restart your computer.\n");
    println!("Opening installer now...");
    
    // Open the installer
    let _ = std::process::Command::new("cmd")
        .args(&["/C", "start", "", installer_path.to_str().unwrap()])
        .spawn()?;
    
    Ok(())
}

#[cfg(feature = "capture")]
#[allow(dead_code)]
/// Provides user-friendly error message if Npcap is not installed
pub fn ensure_npcap_or_error() -> anyhow::Result<()> {
    if !check_npcap_installed() {
        println!("\nNpcap is not installed. Network packet capture requires Npcap.");
        print!("Would you like to download and install Npcap now? (y/n): ");
        
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        
        if input.trim().to_lowercase() == "y" {
            download_and_prompt_npcap_install()?;
            anyhow::bail!(
                "\nPlease complete the Npcap installation and restart your computer.\n\
                Then run: cargo run --features capture -- intercept"
            )
        } else {
            anyhow::bail!(
                "\nTo install Npcap manually:\n\
                1. Download from: https://nmap.org/npcap/\n\
                2. During installation, enable 'WinPcap API-compatible Mode'\n\
                3. Restart your computer after installation\n\
                \n\
                For now, you can use the example command to test the UI:\n\
                cargo run -- example"
            )
        }
    }
    Ok(())
}

#[cfg(not(feature = "capture"))]
pub fn check_npcap_installed() -> bool {
    false
}

#[cfg(not(feature = "capture"))]
pub fn ensure_npcap_or_error() -> anyhow::Result<()> {
    anyhow::bail!(
        "Packet capture requires the 'capture' feature.\n\
        Run with: cargo run --features capture -- intercept\n\
        \n\
        For now, you can use the example command to test the UI:\n\
        cargo run -- example"
    )
}
