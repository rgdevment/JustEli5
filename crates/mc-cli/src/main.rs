//! `eli5` (also installed as `mc`): meeting capture CLI.

use std::process::ExitCode;

use mc_cli::cli::{Env, Io};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let env = Env::from_process();
    let migration = mc_cli::legacy::migrate(&env);
    for line in &migration.lines {
        eprintln!("eli5: {line}");
    }
    if migration.data_moved {
        // The index keeps each session's folder: importing again points it at the new one.
        let (mut out, mut err) = (std::io::sink(), std::io::sink());
        let io = &mut Io {
            out: &mut out,
            err: &mut err,
            input: None,
        };
        mc_cli::cli::run(&["import".to_string()], &env, io);
    }
    // Unlocked: a locked handle would block every other thread's output.
    let (mut out, mut err) = (std::io::stdout(), std::io::stderr());
    let input: Option<Box<dyn std::io::BufRead + Send>> = env
        .stdin_tty
        .then(|| Box::new(std::io::BufReader::new(std::io::stdin())) as _);
    let code = mc_cli::cli::run(
        &args,
        &env,
        &mut Io {
            out: &mut out,
            err: &mut err,
            input,
        },
    );
    ExitCode::from(code)
}
