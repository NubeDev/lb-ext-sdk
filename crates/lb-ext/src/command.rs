//! CLI command parsing + dispatch, kept in one place so `main.rs` stays a thin entry point.

pub const USAGE: &str = "\
usage: lb-ext <command> [args]

commands:
  new <id> --tier <wasm|native> [--features a,b]   scaffold a fresh extension
  build <path>                                     build an extension folder
  pack <path> [--key <keyfile>]                    produce a signed Artifact
  publish <path> --node <url> [--key <keyfile>]    build + pack + POST to a node

  --world-major                                    print the WIT world major and exit";

/// A CLI failure: a usage error (exit 2, prints USAGE) or a runtime failure (exit 1).
pub enum CliError {
    Usage(String),
    Failed(String),
}

/// Parse `args` and dispatch. Returns `Ok(())` on success.
pub fn run(args: &[String]) -> Result<(), CliError> {
    let (cmd, rest) = args
        .split_first()
        .ok_or_else(|| CliError::Usage("no command given".into()))?;

    match cmd.as_str() {
        "--world-major" | "world-major" => {
            println!("{}", lb_sdk::WORLD_MAJOR);
            Ok(())
        }
        "new" => cmd_new(rest),
        "build" => cmd_build(rest),
        "pack" => cmd_pack(rest),
        "publish" => cmd_publish(rest),
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            Ok(())
        }
        other => Err(CliError::Usage(format!("unknown command: {other}"))),
    }
}

fn cmd_new(rest: &[String]) -> Result<(), CliError> {
    let id = positional(rest).ok_or_else(|| CliError::Usage("new: missing <id>".into()))?;
    let tier = flag(rest, "--tier").unwrap_or("wasm");
    if tier != "wasm" && tier != "native" {
        return Err(CliError::Usage(format!(
            "new: --tier must be wasm|native, got {tier}"
        )));
    }
    // TODO(lb-devkit): render the tier template with the requested features (ext-sdk-scope.md).
    println!("would scaffold '{id}' (tier={tier}) via lb-devkit templates");
    Ok(())
}

fn cmd_build(rest: &[String]) -> Result<(), CliError> {
    let path = positional(rest).ok_or_else(|| CliError::Usage("build: missing <path>".into()))?;
    // TODO(lb-devkit): cargo (wasm32-wasip2|release) + vite build via the Toolchain trait.
    println!("would build extension at {path}");
    Ok(())
}

fn cmd_pack(rest: &[String]) -> Result<(), CliError> {
    let path = positional(rest).ok_or_else(|| CliError::Usage("pack: missing <path>".into()))?;
    // TODO(lb-devkit): sign wasm + manifest + ui/dist into a v2 Artifact (Ed25519, ext-out-of-tree).
    println!("would pack + sign a v2 Artifact from {path}");
    Ok(())
}

fn cmd_publish(rest: &[String]) -> Result<(), CliError> {
    let path = positional(rest).ok_or_else(|| CliError::Usage("publish: missing <path>".into()))?;
    let node = flag(rest, "--node")
        .ok_or_else(|| CliError::Usage("publish: missing --node <url>".into()))?;
    // A malformed node URL is a runtime failure (exit 1), not a usage error — POST would never work.
    if !node.starts_with("http://") && !node.starts_with("https://") {
        return Err(CliError::Failed(format!(
            "--node must be an http(s) URL, got {node}"
        )));
    }
    // TODO(lb-devkit): build + pack, then POST /extensions (ext.publish gate) — 204/403/422.
    println!("would publish {path} to {node}");
    Ok(())
}

/// The first argument that is not a flag or a flag's value.
fn positional(rest: &[String]) -> Option<&str> {
    let mut skip_next = false;
    for a in rest {
        if skip_next {
            skip_next = false;
            continue;
        }
        if a.starts_with("--") {
            skip_next = true; // assume `--flag value`
            continue;
        }
        return Some(a);
    }
    None
}

/// The value following `--name`, if present.
fn flag<'a>(rest: &'a [String], name: &str) -> Option<&'a str> {
    rest.iter()
        .position(|a| a == name)
        .and_then(|i| rest.get(i + 1))
        .map(String::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn no_command_is_usage_error() {
        assert!(matches!(run(&[]), Err(CliError::Usage(_))));
    }

    #[test]
    fn unknown_command_is_usage_error() {
        assert!(matches!(run(&s(&["frobnicate"])), Err(CliError::Usage(_))));
    }

    #[test]
    fn world_major_prints_and_succeeds() {
        assert!(run(&s(&["--world-major"])).is_ok());
    }

    #[test]
    fn new_requires_id() {
        assert!(matches!(run(&s(&["new"])), Err(CliError::Usage(_))));
    }

    #[test]
    fn new_rejects_bad_tier() {
        assert!(matches!(
            run(&s(&["new", "x", "--tier", "bogus"])),
            Err(CliError::Usage(_))
        ));
    }

    #[test]
    fn new_accepts_valid_tier() {
        assert!(run(&s(&["new", "cooler", "--tier", "native"])).is_ok());
    }

    #[test]
    fn publish_requires_node() {
        assert!(matches!(
            run(&s(&["publish", "./ext"])),
            Err(CliError::Usage(_))
        ));
    }

    #[test]
    fn publish_rejects_non_http_node() {
        assert!(matches!(
            run(&s(&["publish", "./ext", "--node", "ftp://x"])),
            Err(CliError::Failed(_))
        ));
    }

    #[test]
    fn positional_skips_flags() {
        assert_eq!(
            positional(&s(&["--tier", "wasm", "cooler"])),
            Some("cooler")
        );
    }
}
