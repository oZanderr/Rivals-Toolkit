//! Marvel Rivals pak profile: AES key, compression, mount point, and version constants.

use std::collections::HashMap;
use std::sync::Arc;

use aes::cipher::KeyInit;

/// AES-256 key for containers using the default (all-zero) encryption-key GUID.
pub(crate) const MARVEL_AES_KEY_HEX: &str =
    "0C263D8C22DCB085894899C3A3796383E9BF9DE0CBFB08C9BF2DEF2E84F29D74";

/// Named encryption keys for containers whose TOC header carries a non-zero key GUID.
/// `pakchunkPatch07-Windows` ships under this named key; the GUID bytes are the raw
/// 16 bytes at offset 64 of its .utoc header.
pub(crate) const NAMED_AES_KEYS: &[([u8; 16], &str)] = &[(
    [
        0xb5, 0x3e, 0x53, 0x5f, 0x8f, 0xee, 0xea, 0x44, 0xaa, 0xcc, 0x46, 0xc9, 0xd5, 0xa1, 0xe7,
        0x07,
    ],
    "C1149B1DBECF933C290C328A764427C1CA2AEC6D9EFEBD6A7750EE3FAB0A059E",
)];

/// IoStore compression block size Marvel Rivals expects.
pub(crate) const RIVALS_BLOCK_SIZE: u32 = 0x10000;

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
        repak_key_from_hex(MARVEL_AES_KEY_HEX)
    }

    /// GUID-keyed AES chain for repak: the default key under the zero GUID plus every named key
    /// under its GUID. GUIDs are the little-endian `u128` of the raw header bytes, matching the
    /// pak footer's `EncryptionKeyGuid`, so repak selects the right key per container.
    pub(crate) fn repak_key_chain(self) -> Result<repak::KeyChain, String> {
        let mut chain = repak::KeyChain::default();
        chain.insert(0, repak_key_from_hex(MARVEL_AES_KEY_HEX)?);
        for (guid_bytes, key_hex) in NAMED_AES_KEYS {
            chain.insert(
                u128::from_le_bytes(*guid_bytes),
                repak_key_from_hex(key_hex)?,
            );
        }
        Ok(chain)
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

/// Compute per-file partial encryption limit via BLAKE3(seed + path).
/// The hash path must include the validated mount prefix to match the game engine.
/// (e.g. `"../../../Marvel/"` validates to `"Marvel/"`, giving `"Marvel/Config/..."`).
fn rivals_encrypted_prefix_len(mount_point: &str, path: &str, total_len: usize) -> usize {
    let validated = mount_point
        .find("../../..")
        .map(|i| &mount_point[i + 8..])
        .and_then(|s| s.strip_prefix('/'))
        .unwrap_or("");

    let hash_path = format!("{validated}{path}");

    let mut hasher = blake3::Hasher::new();
    hasher.update(&RIVALS_ENCRYPTION_SEED_BYTES);
    hasher.update(hash_path.to_ascii_lowercase().as_bytes());

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

/// Build an AES-256 key for repak from hex, reversing each 4-byte word to match repak-rivals.
fn repak_key_from_hex(hex: &str) -> Result<aes::Aes256, String> {
    let mut bytes = hex::decode(hex).map_err(|e| e.to_string())?;
    bytes.chunks_mut(4).for_each(|chunk| chunk.reverse());
    aes::Aes256::new_from_slice(&bytes).map_err(|e| e.to_string())
}

/// Deserialize an `FGuid` from its raw 16 little-endian header bytes.
pub(crate) fn guid_from_bytes(bytes: &[u8; 16]) -> Result<retoc::FGuid, String> {
    use retoc::ser::Readable;
    retoc::FGuid::de(&mut std::io::Cursor::new(&bytes[..])).map_err(|e| e.to_string())
}

/// Key used to obfuscate containers we write: the default-GUID key the game already holds, so
/// the runtime decrypts transparently while readers without it see ciphertext.
pub(super) fn obfuscation_key() -> Result<retoc::AesKey, String> {
    MARVEL_AES_KEY_HEX.parse().map_err(|e| format!("{e}"))
}

/// Build a retoc `Config` with every known AES key wired in (the default-GUID key plus
/// any named keys). Shared by every IoStore read path so encrypted containers decrypt
/// consistently.
pub(super) fn make_config() -> Result<Arc<retoc::Config>, String> {
    let principal: retoc::AesKey = MARVEL_AES_KEY_HEX.parse().map_err(|e| format!("{e}"))?;
    let mut aes_keys = HashMap::from([(retoc::FGuid::default(), principal)]);
    for (guid_bytes, key_hex) in NAMED_AES_KEYS {
        let guid = guid_from_bytes(guid_bytes)?;
        let key: retoc::AesKey = key_hex.parse().map_err(|e| format!("{e}"))?;
        aes_keys.insert(guid, key);
    }
    Ok(Arc::new(retoc::Config {
        aes_keys,
        ..Default::default()
    }))
}
