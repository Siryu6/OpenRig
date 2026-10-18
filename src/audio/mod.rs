pub mod chain_switcher;
pub mod engine;
pub mod jack_client;
pub mod looper;
pub mod routing;
pub mod split_chain;
pub mod tuner;

pub use chain_switcher::{ChainControl, ChainSwitcher};
pub use engine::AudioEngine;
pub use looper::Looper;
pub use split_chain::{ChainNode, SplitType};
pub use tuner::{Tuner, TunerResult};

/// Treat denormal floats as zero on this thread (audio, convolution worker):
/// decaying tails (gate, filters, reverbs, IRs) otherwise fall into very slow
/// arithmetic on x86. Two instructions, so the audio thread simply does it at
/// every callback
pub fn flush_denormals() {
    #[cfg(target_arch = "x86_64")]
    // SAFETY: only sets the FTZ and DAZ bits of this thread's MXCSR
    unsafe {
        let mut csr = 0u32;
        std::arch::asm!("stmxcsr [{}]", in(reg) &mut csr, options(nostack));
        csr |= 0x8040;
        std::arch::asm!("ldmxcsr [{}]", in(reg) &csr, options(nostack));
    }
    #[cfg(target_arch = "aarch64")]
    // SAFETY: only sets the FZ bit of this thread's FPCR
    unsafe {
        let fpcr: u64;
        std::arch::asm!("mrs {}, fpcr", out(reg) fpcr, options(nomem, nostack));
        std::arch::asm!("msr fpcr, {}", in(reg) fpcr | (1 << 24), options(nomem, nostack));
    }
}
