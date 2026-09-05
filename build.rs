use std::env;
use std::process::Command;

fn main() {
    if env::var("PLUGIN_VERSION").is_err() {
        let version = env!("CARGO_PKG_VERSION");
        let git_hash = Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .output()
            .ok()
            .and_then(|output| {
                output
                    .status
                    .success()
                    .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
            });

        let app_version = git_hash
            .map_or_else(|| version.to_string(), |hash| format!("{version}-{hash}"));

        println!("cargo:rustc-env=PLUGIN_VERSION={app_version}");
    }

    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads/");
}
