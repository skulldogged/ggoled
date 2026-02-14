use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

const APP_BINARY: &str = "ggoled_app";
const DEFAULT_BUNDLE_ID: &str = "com.apple.ggoled.app";
const DEFAULT_CODESIGN_IDENTITY: &str = "-";

#[derive(Debug, Clone)]
struct BuildMacOptions {
    targets: Vec<String>,
    release: bool,
    sign_com_apple: bool,
    bundle_id: String,
    codesign_identity: String,
}

#[derive(Debug, Clone)]
struct RunMacOptions {
    build: BuildMacOptions,
    app_args: Vec<String>,
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let Some(command) = args.next() else {
        print_usage();
        bail!("missing command");
    };

    match command.as_str() {
        "build-macos" => build_macos(parse_build_macos_args(
            args.collect(),
            vec!["aarch64-apple-darwin".to_string(), "x86_64-apple-darwin".to_string()],
        )?),
        "run-macos" => run_macos(parse_run_macos_args(args.collect())?),
        "help" | "--help" | "-h" => {
            print_usage();
            Ok(())
        }
        _ => {
            print_usage();
            bail!("unknown command: {command}");
        }
    }
}

fn parse_build_macos_args(args: Vec<String>, default_targets: Vec<String>) -> Result<BuildMacOptions> {
    let mut options = BuildMacOptions {
        targets: default_targets,
        release: true,
        sign_com_apple: true,
        bundle_id: DEFAULT_BUNDLE_ID.to_string(),
        codesign_identity: DEFAULT_CODESIGN_IDENTITY.to_string(),
    };

    let mut targets = Vec::new();
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--target" => {
                let target = args.get(i + 1).ok_or_else(|| anyhow!("missing value for --target"))?;
                targets.push(target.to_string());
                i += 2;
            }
            "--debug" => {
                options.release = false;
                i += 1;
            }
            "--bundle-id" => {
                let bundle_id = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("missing value for --bundle-id"))?;
                options.bundle_id = bundle_id.to_string();
                i += 2;
            }
            "--codesign-identity" => {
                let identity = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("missing value for --codesign-identity"))?;
                options.codesign_identity = identity.to_string();
                i += 2;
            }
            "--help" | "-h" => {
                print_build_macos_usage();
                std::process::exit(0);
            }
            other => bail!("unknown build-macos arg: {other}"),
        }
    }

    if !targets.is_empty() {
        options.targets = targets;
    }

    if !options.bundle_id.starts_with("com.apple") {
        bail!("--bundle-id must start with com.apple");
    }

    Ok(options)
}

fn parse_run_macos_args(args: Vec<String>) -> Result<RunMacOptions> {
    let (build_args, app_args) = if let Some(idx) = args.iter().position(|arg| arg == "--") {
        (args[..idx].to_vec(), args[idx + 1..].to_vec())
    } else {
        (args, vec![])
    };

    let build = parse_build_macos_args(build_args, vec![host_macos_target()?.to_string()])?;
    if build.targets.len() != 1 {
        bail!("run-macos requires exactly one --target");
    }

    Ok(RunMacOptions { build, app_args })
}

fn host_macos_target() -> Result<&'static str> {
    if std::env::consts::OS != "macos" {
        bail!("run-macos is only supported on macOS hosts");
    }
    match std::env::consts::ARCH {
        "aarch64" => Ok("aarch64-apple-darwin"),
        "x86_64" => Ok("x86_64-apple-darwin"),
        arch => bail!("unsupported macOS architecture: {arch}"),
    }
}

fn build_macos(options: BuildMacOptions) -> Result<()> {
    for target in &options.targets {
        run_build(target, options.release)?;
        if options.sign_com_apple {
            let binary_path = build_binary_path(target, options.release);
            run_codesign(&binary_path, &options.bundle_id, &options.codesign_identity)?;
        }
    }

    Ok(())
}

fn run_macos(options: RunMacOptions) -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!("run-macos is only supported when xtask runs on macOS");
    }

    build_macos(options.build.clone())?;

    let target = &options.build.targets[0];
    let binary_path = build_binary_path(target, options.build.release);
    let status = Command::new(&binary_path)
        .args(&options.app_args)
        .status()
        .with_context(|| format!("failed to run {}", binary_path.display()))?;
    if !status.success() {
        bail!("{} exited with status {status}", binary_path.display());
    }

    Ok(())
}

fn run_build(target: &str, release: bool) -> Result<()> {
    let mut command = Command::new("cargo");
    command.args(["build", "--locked", "--target", target, "-p", "ggoled_app"]);
    if release {
        command.arg("--release");
    }

    let profile_name = if release { "release" } else { "debug" };
    let status = command
        .status()
        .with_context(|| format!("failed to run cargo build for {target}"))?;
    if !status.success() {
        bail!("cargo build failed for target {target} ({profile_name})");
    }

    Ok(())
}

fn run_codesign(binary_path: &Path, bundle_id: &str, identity: &str) -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!("codesigning is only supported when xtask runs on macOS");
    }
    if !binary_path.exists() {
        bail!("binary not found for codesign: {}", binary_path.display());
    }

    let status = Command::new("codesign")
        .args(["--force", "--sign", identity, "--identifier", bundle_id])
        .arg(binary_path)
        .status()
        .context("failed to execute codesign")?;
    if !status.success() {
        bail!("codesign failed for {}", binary_path.display());
    }

    let status = Command::new("codesign")
        .args(["--verify", "--strict", "--verbose=2"])
        .arg(binary_path)
        .status()
        .context("failed to verify codesign signature")?;
    if !status.success() {
        bail!("codesign verification failed for {}", binary_path.display());
    }

    Ok(())
}

fn build_binary_path(target: &str, release: bool) -> PathBuf {
    let profile = if release { "release" } else { "debug" };
    PathBuf::from("target").join(target).join(profile).join(APP_BINARY)
}

fn print_usage() {
    println!("Usage:");
    println!("  cargo run -p xtask -- build-macos [options]");
    println!("  cargo run -p xtask -- run-macos [options] [-- <ggoled_app args>]");
    println!();
    print_build_macos_usage();
    println!();
    print_run_macos_usage();
}

fn print_build_macos_usage() {
    println!("build-macos options:");
    println!("  --target <triple>            Build target (repeatable)");
    println!("  --debug                      Build debug profile (default: release)");
    println!("  --bundle-id <id>             Bundle id (default: com.apple.ggoled.app)");
    println!("  --codesign-identity <id>     Codesign identity (default: - for ad-hoc)");
}

fn print_run_macos_usage() {
    println!("run-macos options:");
    println!("  --target <triple>            Build/run target (default: host target)");
    println!("  --debug                      Build debug profile (default: release)");
    println!("  --bundle-id <id>             Bundle id (default: com.apple.ggoled.app)");
    println!("  --codesign-identity <id>     Codesign identity (default: - for ad-hoc)");
    println!("  --                           Pass remaining args to ggoled_app");
}
