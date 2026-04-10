use anyhow::Result;
use clap::Parser;
use core::bus::Bus;
use core::cpu::Cpu;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "psoxide", about = "PSoXide-2 — PS1 Emulator")]
struct Args {
    /// Path to PS1 BIOS file (512KB)
    #[arg(short, long)]
    bios: PathBuf,

    /// Path to game disc image (.cue file)
    #[arg(short, long)]
    game: Option<PathBuf>,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    tracing::info!("PSoXide-2 starting");

    let mut bus = Bus::new();
    bus.load_bios(&args.bios)?;

    let mut cpu = Cpu::new();
    cpu.reset();

    // Run for 3 seconds of PS1 time with debug logging
    let target_cycles: u64 = 33_868_800 * 3;
    let mut step_count = 0u64;

    while cpu.regs.cycle < target_cycles {
        cpu.step(&mut bus);
        step_count += 1;

        if step_count % 10_000_000 == 0 {
            eprintln!("[{:10}] PC={:08X} Status={:08X} ISTAT={:08X} IMASK={:08X}",
                step_count, cpu.regs.pc,
                cpu.regs.cp0[12], bus.read_istat(), bus.read_imask());
        }
    }

    eprintln!("=== {} steps, {} cycles ===", step_count, cpu.regs.cycle);
    eprintln!("GPU: {}x{} at ({},{}) GPUSTAT={:08X}",
        bus.gpu.display.width(), bus.gpu.display.height(),
        bus.gpu.display.display_area_x, bus.gpu.display.display_area_y,
        bus.gpu.read_status());
    eprintln!("VRAM non-zero: {}", bus.gpu.vram.data.iter().filter(|&&p| p != 0).count());

    Ok(())
}
