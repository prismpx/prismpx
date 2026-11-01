use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use prismpx::{Effort, MetadataPolicy, Mode, Options, DEFAULT_QUALITY};

const HELP: &str = "\
prismpx - make images smaller without changing them

USAGE:
    prismpx [OPTIONS] <FILE>...
    prismpx score <ORIGINAL> <DISTORTED>   Print the SSIMULACRA2 score (PNG inputs)

Without --output or --in-place, runs as a dry run and only reports savings.

OPTIONS:
    -o, --output <PATH>     Write the result to PATH (single input only)
        --in-place          Overwrite inputs that got smaller
    -l, --lossy             Allow invisible changes for much smaller files
    -q, --quality <SCORE>   Minimum SSIMULACRA2 score with --lossy [default: QUALITY]
                            90 visually lossless, 85 excellent, 80 very high
    -e, --effort <LEVEL>    fast | default | max            [default: default]
    -m, --metadata <MODE>   safe | keep-all | color-only     [default: safe]
        --clean-transparent Zero the color of fully transparent pixels
        --no-verify         Skip decoding the output to check it
        --quiet             Only print errors
    -h, --help              Print help
    -V, --version           Print version
";

struct Args {
    files: Vec<PathBuf>,
    output: Option<PathBuf>,
    in_place: bool,
    quiet: bool,
    opts: Options,
}

fn parse_args() -> Result<Option<Args>, String> {
    let mut args = Args {
        files: Vec::new(),
        output: None,
        in_place: false,
        quiet: false,
        opts: Options::default(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match a.as_str() {
            "-h" | "--help" => {
                print!("{}", HELP.replace("QUALITY", &DEFAULT_QUALITY.to_string()));
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("prismpx {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            "-o" | "--output" => args.output = Some(value(&a)?.into()),
            "--in-place" => args.in_place = true,
            "--quiet" => args.quiet = true,
            "-l" | "--lossy" => args.opts.mode = Mode::VisuallyLossless,
            "-q" | "--quality" => {
                let v = value(&a)?;
                args.opts.quality = v
                    .parse::<f32>()
                    .ok()
                    .filter(|q| (0.0..=100.0).contains(q))
                    .ok_or_else(|| format!("quality must be a number from 0 to 100, got '{v}'"))?;
            }
            "--no-verify" => args.opts.verify = false,
            "--clean-transparent" => args.opts.clean_transparent = true,
            "-e" | "--effort" => {
                args.opts.effort = match value(&a)?.as_str() {
                    "fast" => Effort::Fast,
                    "default" => Effort::Default,
                    "max" => Effort::Max,
                    v => return Err(format!("unknown effort '{v}'")),
                }
            }
            "-m" | "--metadata" => {
                args.opts.metadata = match value(&a)?.as_str() {
                    "safe" => MetadataPolicy::Safe,
                    "keep-all" => MetadataPolicy::KeepAll,
                    "color-only" => MetadataPolicy::ColorOnly,
                    v => return Err(format!("unknown metadata mode '{v}'")),
                }
            }
            s if s.starts_with('-') && s.len() > 1 => return Err(format!("unknown option '{s}'")),
            _ => args.files.push(a.into()),
        }
    }
    if args.files.is_empty() {
        return Err("no input files (see --help)".into());
    }
    if args.output.is_some() && (args.files.len() > 1 || args.in_place) {
        return Err("--output takes exactly one input and conflicts with --in-place".into());
    }
    Ok(Some(args))
}

fn human(n: usize) -> String {
    if n >= 1 << 20 {
        format!("{:.1} MiB", n as f64 / (1 << 20) as f64)
    } else if n >= 1 << 10 {
        format!("{:.1} KiB", n as f64 / 1024.0)
    } else {
        format!("{n} B")
    }
}

fn process(path: &Path, args: &Args) -> Result<(usize, usize), String> {
    let input = std::fs::read(path).map_err(|e| e.to_string())?;
    let start = Instant::now();
    let r = prismpx::optimize(&input, &args.opts).map_err(|e| e.to_string())?;
    let elapsed = start.elapsed();

    let target = args
        .output
        .clone()
        .or_else(|| (args.in_place && r.optimized).then(|| path.to_path_buf()));
    if let Some(t) = &target {
        std::fs::write(t, &r.data).map_err(|e| format!("writing {}: {e}", t.display()))?;
    }
    if !args.quiet {
        let pct = 100.0 * r.saved_bytes() as f64 / input.len().max(1) as f64;
        println!(
            "{}: {} -> {} (-{pct:.1}%) in {:.0?}\n    {}",
            path.display(),
            human(input.len()),
            human(r.size()),
            elapsed,
            r.strategy
        );
    }
    Ok((input.len(), r.size()))
}

/// `prismpx score <original> <distorted>`: prints the SSIMULACRA2 score.
fn score_command(files: &[String]) -> ExitCode {
    if files.len() != 2 {
        eprintln!("usage: prismpx score <original.png> <distorted.png>");
        return ExitCode::from(2);
    }
    let load = |p: &str| -> Result<prismpx::png::Image, String> {
        let data = std::fs::read(p).map_err(|e| format!("{p}: {e}"))?;
        prismpx::png::decode(&data).map_err(|e| format!("{p}: {e}"))
    };
    match (load(&files[0]), load(&files[1])) {
        (Ok(a), Ok(b)) => match prismpx::metrics::ssimulacra2(&a, &b) {
            Ok(s) => {
                println!("{s:.8}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.first().map(String::as_str) == Some("score") {
        return score_command(&raw[1..]);
    }
    let args = match parse_args() {
        Ok(Some(a)) => a,
        Ok(None) => return ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    let dry_run = args.output.is_none() && !args.in_place;
    let (mut before, mut after, mut failed) = (0usize, 0usize, 0);
    for f in &args.files {
        match process(f, &args) {
            Ok((b, a)) => {
                before += b;
                after += a;
            }
            Err(e) => {
                eprintln!("{}: {e}", f.display());
                failed += 1;
            }
        }
    }
    if !args.quiet && args.files.len() > 1 {
        println!(
            "total: {} -> {} (-{:.1}%){}",
            human(before),
            human(after),
            100.0 * (before - after) as f64 / before.max(1) as f64,
            if dry_run { " [dry run]" } else { "" }
        );
    } else if !args.quiet && dry_run {
        println!("[dry run: use --output or --in-place to write]");
    }
    if failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
