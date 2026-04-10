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
    tracing::info!("BIOS: {}", args.bios.display());

    let mut bus = Bus::new();
    bus.load_bios(&args.bios)?;

    let mut cpu = Cpu::new();
    cpu.reset();

    tracing::info!("Running BIOS...");

    // Run BIOS, detect tight loop and dump it
    let mut step_count = 0u64;
    let mut pc_history: Vec<u32> = Vec::new();

    loop {
        let pc = cpu.regs.pc;
        cpu.step(&mut bus);
        step_count += 1;

        pc_history.push(pc);
        if pc_history.len() > 100 { pc_history.remove(0); }

        // Report at milestones
        if step_count % 5_000_000 == 0 {
            eprintln!("[{:8}] PC={:08X} Status={:08X} ISTAT={:08X} IMASK={:08X} cycle={}",
                step_count, cpu.regs.pc,
                cpu.regs.cp0[12], bus.read_istat(), bus.read_imask(), cpu.regs.cycle);
        }

        if step_count >= 50_000_000 {
            eprintln!("=== DONE at {} steps, PC={:08X} ===", step_count, cpu.regs.pc);
            // Dump GPU state
            eprintln!("GPU: display={}x{} at ({},{})",
                bus.gpu.display.width(), bus.gpu.display.height(),
                bus.gpu.display.display_area_x, bus.gpu.display.display_area_y);
            eprintln!("GPUSTAT: {:08X}", bus.gpu.read_status());

            // Check if VRAM has any non-zero pixels
            let mut nonzero = 0;
            for &p in bus.gpu.vram.data.iter() {
                if p != 0 { nonzero += 1; }
            }
            eprintln!("VRAM non-zero pixels: {}", nonzero);
            break;
        }
    }

    Ok(())
}
