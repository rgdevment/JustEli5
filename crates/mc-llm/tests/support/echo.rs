//! Test helper: prints its arguments and stdin; `--fail` exits 3, `--sleep` hangs,
//! `--orphan` leaves a sleeping grandchild holding the output pipes and exits.

use std::io::Read;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut stdin = String::new();
    let _ = std::io::stdin().read_to_string(&mut stdin);
    if args.iter().any(|a| a == "--orphan") {
        let me = std::env::current_exe().expect("own path");
        let grandchild = std::process::Command::new(me)
            .arg("--sleep")
            .spawn()
            .expect("grandchild");
        // Dropped without waiting: the parent exits while the grandchild lives on.
        std::mem::forget(grandchild);
        return;
    }
    if args.iter().any(|a| a == "--sleep") {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
    if args.iter().any(|a| a == "--fail") {
        eprintln!("boom");
        std::process::exit(3);
    }
    println!("ARGS:{}", args.join("|"));
    println!("STDIN:{stdin}");
}
