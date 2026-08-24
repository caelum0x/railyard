//! Row encode/decode helpers: BIGINT µ$ ↔ [`Micros`], smallint ↔ enums, bytea ↔
//! 32/64-byte hashes/sigs. NO floats anywhere — every amount is an `i64`.

use mint_core::{AccountKind, Micros, OpHash};
use mint_receipts::{ReceiptHash, ReceiptOp, ReceiptSig};
use mint_store::{
    AllowanceScheme, AllowanceWindow, Bucket, HoldStatus, OpKind, StreamStatus, StreamUnit,
};

/// Decode a `BIGINT` µ$ column into a [`Micros`].
pub fn micros(v: i64) -> Micros {
    Micros::new(v)
}

// ---- AccountKind <-> smallint (the stable nibble) --------------------------

pub fn kind_to_i16(k: AccountKind) -> i16 {
    k.to_nibble() as i16
}

pub fn kind_from_i16(v: i16) -> Option<AccountKind> {
    u8::try_from(v).ok().and_then(AccountKind::from_nibble)
}

// ---- Bucket <-> smallint ---------------------------------------------------

pub fn bucket_to_i16(b: Bucket) -> i16 {
    match b {
        Bucket::Available => 0,
        Bucket::Held => 1,
    }
}

pub fn bucket_from_i16(v: i16) -> Bucket {
    match v {
        1 => Bucket::Held,
        _ => Bucket::Available,
    }
}

// ---- HoldStatus <-> smallint -----------------------------------------------

pub fn hold_status_to_i16(s: HoldStatus) -> i16 {
    match s {
        HoldStatus::Open => 0,
        HoldStatus::Captured => 1,
        HoldStatus::Released => 2,
    }
}

pub fn hold_status_from_i16(v: i16) -> HoldStatus {
    match v {
        1 => HoldStatus::Captured,
        2 => HoldStatus::Released,
        _ => HoldStatus::Open,
    }
}

// ---- StreamStatus <-> smallint ---------------------------------------------

pub fn stream_status_to_i16(s: StreamStatus) -> i16 {
    match s {
        StreamStatus::Open => 0,
        StreamStatus::Closed => 1,
    }
}

pub fn stream_status_from_i16(v: i16) -> StreamStatus {
    match v {
        1 => StreamStatus::Closed,
        _ => StreamStatus::Open,
    }
}

// ---- StreamUnit <-> smallint -----------------------------------------------

pub fn stream_unit_to_i16(u: StreamUnit) -> i16 {
    match u {
        StreamUnit::Second => 0,
        StreamUnit::Unit => 1,
    }
}

pub fn stream_unit_from_i16(v: i16) -> StreamUnit {
    match v {
        1 => StreamUnit::Unit,
        _ => StreamUnit::Second,
    }
}

// ---- AllowanceScheme <-> smallint ------------------------------------------

pub fn allowance_scheme_to_i16(s: AllowanceScheme) -> i16 {
    match s {
        AllowanceScheme::Transfer => 0,
        AllowanceScheme::Hold => 1,
    }
}

// ---- AllowanceWindow <-> (smallint kind, bigint window_ms) -----------------

pub fn window_to_cols(w: AllowanceWindow) -> (i16, i64) {
    match w {
        AllowanceWindow::PerCall => (0, 0),
        AllowanceWindow::Rolling { window_ms } => (1, window_ms),
    }
}

pub fn window_from_cols(kind: i16, window_ms: i64) -> AllowanceWindow {
    match kind {
        1 => AllowanceWindow::Rolling { window_ms },
        _ => AllowanceWindow::PerCall,
    }
}

// ---- OpKind <-> text -------------------------------------------------------

pub fn op_to_text(op: OpKind) -> &'static str {
    match op {
        OpKind::Transfer => "transfer",
        OpKind::Seed => "seed",
        OpKind::HoldCreate => "hold.create",
        OpKind::HoldCapture => "hold.capture",
        OpKind::HoldRelease => "hold.release",
        OpKind::StreamOpen => "stream.open",
        OpKind::StreamTick => "stream.tick",
        OpKind::StreamClose => "stream.close",
        OpKind::Mint => "mint",
        OpKind::Redeem => "redeem",
    }
}

pub fn op_from_text(s: &str) -> OpKind {
    match s {
        "transfer" => OpKind::Transfer,
        "hold.create" => OpKind::HoldCreate,
        "hold.capture" => OpKind::HoldCapture,
        "hold.release" => OpKind::HoldRelease,
        "stream.open" => OpKind::StreamOpen,
        "stream.tick" => OpKind::StreamTick,
        "stream.close" => OpKind::StreamClose,
        "mint" => OpKind::Mint,
        "redeem" => OpKind::Redeem,
        _ => OpKind::Seed,
    }
}

// ---- ReceiptOp <-> text ----------------------------------------------------

pub fn receipt_op_to_text(op: ReceiptOp) -> &'static str {
    match op {
        ReceiptOp::Seed => "seed",
        ReceiptOp::Transfer => "transfer",
        ReceiptOp::HoldCreate => "hold.create",
        ReceiptOp::HoldCapture => "hold.capture",
        ReceiptOp::HoldRelease => "hold.release",
        ReceiptOp::StreamOpen => "stream.open",
        ReceiptOp::StreamTick => "stream.tick",
        ReceiptOp::StreamClose => "stream.close",
        ReceiptOp::Mint => "mint",
        ReceiptOp::Redeem => "redeem",
    }
}

pub fn receipt_op_from_text(s: &str) -> ReceiptOp {
    match s {
        "transfer" => ReceiptOp::Transfer,
        "hold.create" => ReceiptOp::HoldCreate,
        "hold.capture" => ReceiptOp::HoldCapture,
        "hold.release" => ReceiptOp::HoldRelease,
        "stream.open" => ReceiptOp::StreamOpen,
        "stream.tick" => ReceiptOp::StreamTick,
        "stream.close" => ReceiptOp::StreamClose,
        "mint" => ReceiptOp::Mint,
        "redeem" => ReceiptOp::Redeem,
        _ => ReceiptOp::Seed,
    }
}

// ---- bytea <-> fixed hash/sig types ----------------------------------------

pub fn hash_to_vec(h: &ReceiptHash) -> Vec<u8> {
    h.as_bytes().to_vec()
}

pub fn ophash_to_vec(h: &OpHash) -> Vec<u8> {
    h.as_bytes().to_vec()
}

pub fn sig_to_vec(s: &ReceiptSig) -> Vec<u8> {
    s.as_bytes().to_vec()
}

/// Decode a `bytea` into a 32-byte [`ReceiptHash`] (zero-padded/truncated to 32).
pub fn hash_from_slice(b: &[u8]) -> ReceiptHash {
    let mut out = [0u8; 32];
    let n = b.len().min(32);
    out[..n].copy_from_slice(&b[..n]);
    ReceiptHash::from_bytes(out)
}

/// Decode a `bytea` into a 32-byte [`OpHash`].
pub fn ophash_from_slice(b: &[u8]) -> OpHash {
    let mut out = [0u8; 32];
    let n = b.len().min(32);
    out[..n].copy_from_slice(&b[..n]);
    OpHash::from_bytes(out)
}

/// Decode a `bytea` into a 64-byte [`ReceiptSig`].
pub fn sig_from_slice(b: &[u8]) -> ReceiptSig {
    let mut out = [0u8; 64];
    let n = b.len().min(64);
    out[..n].copy_from_slice(&b[..n]);
    ReceiptSig::from_bytes(out)
}

#[cfg(test)]
mod tests {
    //! DB-free unit tests for the pure row encode/decode mappers. These need no
    //! live `DATABASE_URL` — they prove the SQL column ↔ domain-type conversions
    //! round-trip exactly (the correctness contract every `commit_*` relies on).

    use super::*;
    use mint_core::OpHash;
    use mint_receipts::{ReceiptHash, ReceiptSig};

    #[test]
    fn account_kind_round_trips_all_variants() {
        for k in [
            AccountKind::User,
            AccountKind::Org,
            AccountKind::Agent,
            AccountKind::Provider,
            AccountKind::Model,
            AccountKind::Treasury,
        ] {
            assert_eq!(kind_from_i16(kind_to_i16(k)), Some(k));
        }
    }

    #[test]
    fn account_kind_from_out_of_range_nibble_is_none() {
        assert_eq!(kind_from_i16(6), None);
        assert_eq!(kind_from_i16(-1), None);
        assert_eq!(kind_from_i16(9999), None);
    }

    #[test]
    fn bucket_round_trips_and_defaults_available() {
        assert_eq!(
            bucket_from_i16(bucket_to_i16(Bucket::Available)),
            Bucket::Available
        );
        assert_eq!(bucket_from_i16(bucket_to_i16(Bucket::Held)), Bucket::Held);
        assert_eq!(bucket_to_i16(Bucket::Available), 0);
        assert_eq!(bucket_to_i16(Bucket::Held), 1);
        // Any unknown smallint decodes to the safe `Available` default.
        assert_eq!(bucket_from_i16(7), Bucket::Available);
    }

    #[test]
    fn hold_status_round_trips() {
        for s in [HoldStatus::Open, HoldStatus::Captured, HoldStatus::Released] {
            assert_eq!(hold_status_from_i16(hold_status_to_i16(s)), s);
        }
        assert_eq!(hold_status_to_i16(HoldStatus::Open), 0);
        assert_eq!(hold_status_to_i16(HoldStatus::Captured), 1);
        assert_eq!(hold_status_to_i16(HoldStatus::Released), 2);
        assert_eq!(hold_status_from_i16(42), HoldStatus::Open);
    }

    #[test]
    fn stream_status_and_unit_round_trip() {
        for s in [StreamStatus::Open, StreamStatus::Closed] {
            assert_eq!(stream_status_from_i16(stream_status_to_i16(s)), s);
        }
        for u in [StreamUnit::Second, StreamUnit::Unit] {
            assert_eq!(stream_unit_from_i16(stream_unit_to_i16(u)), u);
        }
        assert_eq!(stream_status_from_i16(99), StreamStatus::Open);
        assert_eq!(stream_unit_from_i16(99), StreamUnit::Second);
    }

    #[test]
    fn allowance_scheme_encodes_stably() {
        assert_eq!(allowance_scheme_to_i16(AllowanceScheme::Transfer), 0);
        assert_eq!(allowance_scheme_to_i16(AllowanceScheme::Hold), 1);
    }

    #[test]
    fn allowance_window_round_trips_both_kinds() {
        let per_call = AllowanceWindow::PerCall;
        let (k, ms) = window_to_cols(per_call);
        assert_eq!((k, ms), (0, 0));
        assert_eq!(window_from_cols(k, ms), per_call);

        let rolling = AllowanceWindow::Rolling { window_ms: 60_000 };
        let (k, ms) = window_to_cols(rolling);
        assert_eq!((k, ms), (1, 60_000));
        assert_eq!(window_from_cols(k, ms), rolling);

        // Unknown kind decodes to the safe cumulative default.
        assert_eq!(window_from_cols(9, 5), AllowanceWindow::PerCall);
    }

    #[test]
    fn op_kind_text_round_trips_all_variants() {
        for op in [
            OpKind::Transfer,
            OpKind::Seed,
            OpKind::HoldCreate,
            OpKind::HoldCapture,
            OpKind::HoldRelease,
            OpKind::StreamOpen,
            OpKind::StreamTick,
            OpKind::StreamClose,
            OpKind::Mint,
            OpKind::Redeem,
        ] {
            assert_eq!(op_from_text(op_to_text(op)), op);
        }
        // Unrecognized op text decodes to the `Seed` control-plane default.
        assert_eq!(op_from_text("bogus"), OpKind::Seed);
    }

    #[test]
    fn receipt_op_text_round_trips_all_variants() {
        for op in [
            ReceiptOp::Seed,
            ReceiptOp::Transfer,
            ReceiptOp::HoldCreate,
            ReceiptOp::HoldCapture,
            ReceiptOp::HoldRelease,
            ReceiptOp::StreamOpen,
            ReceiptOp::StreamTick,
            ReceiptOp::StreamClose,
            ReceiptOp::Mint,
            ReceiptOp::Redeem,
        ] {
            assert_eq!(receipt_op_from_text(receipt_op_to_text(op)), op);
        }
        assert_eq!(receipt_op_from_text("bogus"), ReceiptOp::Seed);
    }

    #[test]
    fn micros_decode_preserves_sign_and_magnitude() {
        assert_eq!(micros(0).get(), 0);
        assert_eq!(micros(3_000_000).get(), 3_000_000);
        assert_eq!(micros(-7).get(), -7);
        assert_eq!(micros(i64::MAX).get(), i64::MAX);
        assert_eq!(micros(i64::MIN).get(), i64::MIN);
    }

    #[test]
    fn op_hash_bytea_round_trips_exactly() {
        let mut raw = [0u8; 32];
        for (i, b) in raw.iter_mut().enumerate() {
            *b = i as u8;
        }
        let h = OpHash::from_bytes(raw);
        let encoded = ophash_to_vec(&h);
        assert_eq!(encoded.len(), 32);
        assert_eq!(ophash_from_slice(&encoded), h);
    }

    #[test]
    fn receipt_hash_bytea_round_trips_exactly() {
        let mut raw = [0u8; 32];
        for (i, b) in raw.iter_mut().enumerate() {
            *b = (255 - i) as u8;
        }
        let h = ReceiptHash::from_bytes(raw);
        let encoded = hash_to_vec(&h);
        assert_eq!(encoded.len(), 32);
        assert_eq!(hash_from_slice(&encoded), h);
    }

    #[test]
    fn receipt_sig_bytea_round_trips_exactly() {
        let mut raw = [0u8; 64];
        for (i, b) in raw.iter_mut().enumerate() {
            *b = (i * 3 % 251) as u8;
        }
        let s = ReceiptSig::from_bytes(raw);
        let encoded = sig_to_vec(&s);
        assert_eq!(encoded.len(), 64);
        assert_eq!(sig_from_slice(&encoded), s);
    }

    #[test]
    fn hash_decoders_zero_pad_short_and_truncate_long_input() {
        // Short input → zero-padded to the fixed width (defends against a
        // corrupt/undersized bytea without panicking).
        let short = [1u8, 2, 3];
        let h = hash_from_slice(&short);
        let mut want = [0u8; 32];
        want[..3].copy_from_slice(&short);
        assert_eq!(h.as_bytes(), &want);

        // Over-long input → truncated to the fixed width (no slice panic).
        let long = vec![9u8; 100];
        let sig = sig_from_slice(&long);
        assert_eq!(sig.as_bytes(), &[9u8; 64]);
        assert_eq!(sig.as_bytes().len(), 64);
    }

    #[test]
    fn receipt_hash_zero_round_trips_through_bytea() {
        let z = ReceiptHash::ZERO;
        assert_eq!(hash_from_slice(&hash_to_vec(&z)), z);
    }
}
