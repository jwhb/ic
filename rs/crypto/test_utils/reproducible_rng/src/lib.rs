use rand::{CryptoRng, Error, Rng, RngCore, SeedableRng};
use rand_chacha::ChaCha20Rng;

/// Provides a seeded RNG, where the randomly chosen seed is printed on standard output.
pub fn reproducible_rng() -> ReproducibleRng {
    ReproducibleRng::new()
}

/// Wraps the logic of [`reproducible_rng`] into a separate struct.
///
/// This is needed when [`reproducible_rng`] cannot be used because its
/// return type `impl Rng + CryptoRng` can only be used as function parameter
/// or as return type
/// (See [impl trait type](https://doc.rust-lang.org/reference/types/impl-trait.html)).
#[derive(Clone)]
pub struct ReproducibleRng {
    rng: ChaCha20Rng,
    seed: [u8; 32],
}

impl ReproducibleRng {
    /// Verbose constructor. Randomly generates a seed and prints it to `stdout`.
    pub fn new() -> Self {
        let rng = Self::silent_new();
        println!("{rng:?}");
        rng
    }

    /// Silent constructor. Randomly generates a seed but doesn't automatically print it to `stdout`.
    pub fn silent_new() -> Self {
        let mut seed = [0u8; 32];
        rand::thread_rng().fill(&mut seed);
        Self::from_seed(seed)
    }
}

impl SeedableRng for ReproducibleRng {
    type Seed = [u8; 32];
    #[inline]
    fn from_seed(seed: Self::Seed) -> Self {
        let rng = ChaCha20Rng::from_seed(seed);
        Self { rng, seed }
    }
}

impl std::fmt::Debug for ReproducibleRng {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Copy the seed below to reproduce the failed test.\n
    let seed: [u8; 32] = {:?};",
            self.seed
        )
    }
}

impl Default for ReproducibleRng {
    fn default() -> Self {
        Self::new()
    }
}

impl RngCore for ReproducibleRng {
    fn next_u32(&mut self) -> u32 {
        self.rng.next_u32()
    }

    fn next_u64(&mut self) -> u64 {
        self.rng.next_u64()
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        self.rng.fill(dest)
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), Error> {
        self.rng.try_fill_bytes(dest)
    }
}

impl CryptoRng for ReproducibleRng {}
