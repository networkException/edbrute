use anyhow::Context;
use ed25519_dalek::{Keypair, PublicKey, SecretKey};
use indicatif::{ProgressBar, ProgressState, ProgressStyle};
use clap::Parser;
use std::{
    fs::File,
    io::{BufRead, BufReader, Write},
    path::Path,
    sync::mpsc::{sync_channel, Receiver, SyncSender},
    time::Duration,
};

#[allow(clippy::large_enum_variant)]
enum WorkerMessage {
    Largest(Keypair),
    Progress { iteration_delta: usize },
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
   /// The number of threads to use
   #[arg(short, long)]
   jobs: Option<usize>,
}

fn main() {
    if let Err(e) = run_main() {
        eprintln!("error running edbrute: {e}");
    }
}

fn run_main() -> anyhow::Result<()> {
    let args = Args::parse();
    let num_threads = args.jobs.unwrap_or(num_cpus::get());

    println!("bruteforcing with {num_threads} thread{}", if num_threads == 1 { "" } else { "s" });

    let mut to_threads = Vec::new();
    let (to_controller, from_threads) = sync_channel(64);

    for _ in 0..num_threads {
        let (to_thread, from_controller) = sync_channel(64);
        to_threads.push(to_thread);

        let to_controller = to_controller.clone();

        std::thread::spawn(move || {
            run_worker(from_controller, to_controller);
        });
        std::thread::sleep(Duration::from_millis(100));
    }

    run_controller(to_threads, from_threads).context("unable to start controller thread")?;

    Ok(())
}

fn run_controller(
    to_threads: Vec<SyncSender<u128>>,
    from_threads: Receiver<WorkerMessage>,
) -> anyhow::Result<()> {
    let spinner = setup_spinner();

    let (mut checkpoint_file, saved_largest_keypair) =
        checkpoint_with_largest_keypair("checkpoint.log")
            .context("unable to create checkpoint file")?;

    let mut largest_keypair =
        saved_largest_keypair.unwrap_or_else(|| Keypair::generate(&mut rand::thread_rng()));

    let mut largest_value = public_key_to_u128(&largest_keypair);
    for sender in &to_threads {
        sender.send(largest_value).unwrap();
    }

    let public_pretty = pretty_print_public(&largest_keypair);
    spinner.set_message(public_pretty);

    while let Ok(keypair) = from_threads.recv() {
        match keypair {
            WorkerMessage::Largest(keypair) => {
                let value = public_key_to_u128(&keypair);

                if value > largest_value {
                    largest_value = value;

                    for sender in &to_threads {
                        sender.send(largest_value).unwrap();
                    }

                    largest_keypair = keypair;

                    writeln!(checkpoint_file, "{}", serialize_keypair(&largest_keypair))
                        .context("unable to save keypair to checkpoint file")?;
                    checkpoint_file.flush()?;

                    let printed_keypair = pretty_print_public(&largest_keypair);
                    spinner.println(format!(
                        "[{}] {}",
                        chrono::Utc::now().format("%Y-%m-%d %H:%M:%S"),
                        &printed_keypair
                    ));
                    spinner.set_message(printed_keypair);
                }
            }
            WorkerMessage::Progress { iteration_delta } => {
                spinner.inc(iteration_delta as u64);
            }
        }
    }

    Ok(())
}

fn run_worker(from_controller: Receiver<u128>, to_controller: SyncSender<WorkerMessage>) {
    let mut rng = rand::thread_rng();
    let mut largest_value = from_controller.recv().unwrap();

    let iteration_delta = u16::MAX as usize;
    loop {
        for _ in 0..iteration_delta {
            let pair = Keypair::generate(&mut rng);
            let value = public_key_to_u128(&pair);

            if value > largest_value {
                to_controller.send(WorkerMessage::Largest(pair)).unwrap();
                largest_value = value;
            }
        }

        if let Ok(largest_found) = from_controller.try_recv() {
            largest_value = largest_found;
        }

        if to_controller
            .send(WorkerMessage::Progress { iteration_delta })
            .is_err()
        {
            break;
        }
    }
}

fn pretty_print_public(keypair: &Keypair) -> String {
    hex::encode(keypair.public)
}

fn serialize_keypair(keypair: &Keypair) -> String {
    format!(
        "{},{}",
        hex::encode(keypair.public.as_bytes()),
        hex::encode(keypair.secret.as_bytes())
    )
}

fn public_key_to_u128(keypair: &Keypair) -> u128 {
    u128::from_be_bytes(keypair.public.as_bytes()[0..16].try_into().unwrap())
}

fn checkpoint_with_largest_keypair(
    path: impl AsRef<Path>,
) -> anyhow::Result<(File, Option<Keypair>)> {
    let checkpoint_file = std::fs::File::options()
        .create(true)
        .read(true)
        .append(true)
        .open(path)
        .context("unable to open checkpoint file")?;

    let reader = BufReader::new(&checkpoint_file);

    let mut keypairs = Vec::new();
    for line in reader.lines().flatten() {
        let (public_hex, secret_hex) = line.split_once(',').context("malformed keypair line")?;
        let (public_bytes, secret_bytes) = (hex::decode(public_hex)?, hex::decode(secret_hex)?);
        let (public, secret) = (
            PublicKey::from_bytes(&public_bytes)?,
            SecretKey::from_bytes(&secret_bytes)?,
        );

        keypairs.push(Keypair { public, secret })
    }

    let starting_key = keypairs.into_iter().max_by_key(public_key_to_u128);
    Ok((checkpoint_file, starting_key))
}

fn setup_spinner() -> ProgressBar {
    let spinner = indicatif::ProgressBar::new_spinner();

    spinner.enable_steady_tick(Duration::from_millis(150));
    spinner.set_style(
        ProgressStyle::with_template(
            "\n{spinner} [{elapsed_precise}] {smoothed_per_sec}, {human_pos} total.\n  largest: {msg}",
        )
        .unwrap()
        .with_key(
            "smoothed_per_sec",
            |s: &ProgressState, w: &mut dyn std::fmt::Write| match (
                s.pos(),
                s.elapsed().as_millis(),
            ) {
                (pos, elapsed_ms) if elapsed_ms > 0 => {
                    write!(w, "{:.2} keys/s", pos as f64 * 1000_f64 / elapsed_ms as f64).unwrap()
                }
                _ => write!(w, "-").unwrap(),
            },
        ),
    );

    spinner
}
