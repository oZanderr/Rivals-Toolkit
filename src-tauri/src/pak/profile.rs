use aes::cipher::KeyInit;

/// AES-256 key used by Marvel Rivals pak files.
const MARVEL_AES_KEY_HEX: &str = "0C263D8C22DCB085894899C3A3796383E9BF9DE0CBFB08C9BF2DEF2E84F29D74";

pub(crate) const RIVALS_MOUNT_POINT: &str = "../../../";
const RIVALS_ENCRYPTION_SEED_BYTES: [u8; 4] = [0x11, 0x22, 0x33, 0x44];
const RIVALS_INDEX_TRAILER: &[u8] = &[
    0x06, 0x12, 0x24, 0x20, 0x06, 0x00, 0x00, 0x00, 0x01, 0x02, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00,
];

#[derive(Debug, Clone, Copy)]
pub(crate) struct RivalsPakProfile;

pub(crate) const RIVALS_PROFILE: RivalsPakProfile = RivalsPakProfile;

impl RivalsPakProfile {
    pub(crate) fn pak_version(self) -> repak::Version {
        repak::Version::V11
    }

    pub(crate) fn compression(self) -> [repak::Compression; 1] {
        [repak::Compression::Oodle]
    }

    pub(crate) fn mount_point(self) -> &'static str {
        RIVALS_MOUNT_POINT
    }

    pub(crate) fn make_aes_key(self) -> Result<aes::Aes256, String> {
        let mut bytes = hex::decode(MARVEL_AES_KEY_HEX).map_err(|e| e.to_string())?;

        // Match repak-rivals key parsing by reversing each 4-byte word.
        bytes.chunks_mut(4).for_each(|chunk| chunk.reverse());

        aes::Aes256::new_from_slice(&bytes).map_err(|e| e.to_string())
    }

    pub(crate) fn repak_profile(self) -> repak::PakProfile {
        repak::PakProfile {
            encrypt_prefix: rivals_encrypted_prefix_len,
            reverse_word_order: true,
            index_trailer: RIVALS_INDEX_TRAILER,
        }
    }

    pub(crate) fn strip_mount_prefix(self, path: &str) -> String {
        path.strip_prefix(self.mount_point())
            .unwrap_or(path)
            .trim_start_matches('/')
            .to_string()
    }
}

/// Partial encryption: only the first N bytes of each file are encrypted, where N is
/// derived from a BLAKE3 hash of the lowercase file path with a game-specific seed.
/// The mount point is ignored — the plain relative path drives the hash.
fn rivals_encrypted_prefix_len(_mount_point: &str, path: &str, total_len: usize) -> usize {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&RIVALS_ENCRYPTION_SEED_BYTES);
    hasher.update(path.to_ascii_lowercase().as_bytes());

    let hash = hasher.finalize();
    let bytes = hash.as_bytes();
    let mut first_u64_bytes = [0u8; 8];
    first_u64_bytes.copy_from_slice(&bytes[..8]);
    let first_u64 = u64::from_le_bytes(first_u64_bytes);

    let limit = ((first_u64 % 0x3D) * 63 + 319) & 0xFFFFFFFFFFFFFFC0;
    let limit = if limit == 0 { 0x1000 } else { limit as usize };

    limit.min(total_len)
}

pub(crate) fn strip_mount_prefix(path: &str) -> String {
    RIVALS_PROFILE.strip_mount_prefix(path)
}
