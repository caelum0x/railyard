//! [`StreamLedger`] — the store-agnostic streaming facade.

use mint_core::{op_hash, Micros, MintError, StreamId, TransferAmount};
use mint_store::{
    LedgerStore, StreamCloseCommit, StreamOpenCommit, StreamRecord, StreamTickCommit,
};
use serde_json::json;

use crate::{
    CloseStreamRequest, OpenStreamRequest, StreamCloseReceipt, StreamOpenReceipt,
    StreamTickReceipt, TickStreamRequest,
};

/// Orchestration facade generic over any [`LedgerStore`]. Owns its store (like
/// `mint-ledger::Ledger`); share one store across facades by operating on the
/// store's trait methods directly.
pub struct StreamLedger<S: LedgerStore> {
    store: S,
}

impl<S: LedgerStore> StreamLedger<S> {
    /// Wrap a store.
    pub fn new(store: S) -> Self {
        StreamLedger { store }
    }

    /// Borrow the underlying store (for seeding / rebuild / balance reads).
    pub fn store(&self) -> &S {
        &self.store
    }

    /// Open a metered stream, reserving the whole `cap` from `from`
    /// (available → held). The `op_hash` EXCLUDES the stream id (the store mints
    /// it), so an idempotent replay returns the SAME original stream.
    ///
    /// - `rate_micros` and `cap_micros` must be strictly positive
    ///   ([`MintError::InvalidAmount`]);
    /// - `from == to` is [`MintError::BadAccountId`];
    /// - insufficient available is [`MintError::InsufficientFunds`].
    pub async fn open_stream(
        &self,
        req: OpenStreamRequest,
    ) -> Result<StreamOpenReceipt, MintError> {
        let rate = TransferAmount::new(Micros::new(req.rate_micros))?;
        let cap = TransferAmount::new(Micros::new(req.cap_micros))?;
        if req.from == req.to {
            return Err(MintError::BadAccountId);
        }
        let hash = op_hash(&json!({
            "op": "stream.open",
            "from": req.from.as_str(),
            "to": req.to.as_str(),
            "rate_micros": rate.get(),
            "cap_micros": cap.get(),
            "unit": req.unit,
            "idem_key": req.idem_key,
            "ref": req.ref_,
        }));
        let out = self
            .store
            .commit_stream_open(StreamOpenCommit {
                from: req.from,
                to: req.to,
                rate,
                cap,
                unit: req.unit,
                idem_key: req.idem_key,
                op_hash: hash,
                ref_: req.ref_,
            })
            .await?;
        Ok(StreamOpenReceipt {
            stream_id: out.stream_id,
            first_seq: out.first_seq,
            holder_balance: out.holder_balance,
            idempotent_replay: out.idempotent_replay,
        })
    }

    /// Settle one stream tick (`units * rate` moved held payer → available
    /// payee). Enforces monotonic `seq` and the cap.
    ///
    /// - `units == 0` is [`MintError::InvalidAmount`];
    /// - a stream that no longer exists is [`MintError::StreamNotFound`];
    /// - a closed stream is [`MintError::StreamClosed`];
    /// - `seq <= last_seq` is [`MintError::StreamSeqRegressed`];
    /// - overshooting the cap is [`MintError::StreamCapExceeded`] (no partial
    ///   application).
    pub async fn tick_stream(
        &self,
        req: TickStreamRequest,
    ) -> Result<StreamTickReceipt, MintError> {
        let stream_id = StreamId::parse(&req.stream_id)?;
        let hash = op_hash(&json!({
            "op": "stream.tick",
            "stream_id": stream_id.as_str(),
            "units": req.units,
            "seq": req.seq,
            "idem_key": req.idem_key,
            "ref": req.ref_,
        }));
        let out = self
            .store
            .commit_stream_tick(StreamTickCommit {
                stream_id,
                units: req.units,
                seq: req.seq,
                idem_key: req.idem_key,
                op_hash: hash,
                ref_: req.ref_,
            })
            .await?;
        Ok(StreamTickReceipt {
            stream_id: out.stream_id,
            tick_seq: out.tick_seq,
            settled_total_micros: out.settled_total_micros,
            from_balance: out.from_balance,
            to_balance: out.to_balance,
            idempotent_replay: out.idempotent_replay,
        })
    }

    /// Close a stream: optionally settle a final tick (`units > 0`), then release
    /// the unused remainder back to the payer's available. Terminal.
    pub async fn close_stream(
        &self,
        req: CloseStreamRequest,
    ) -> Result<StreamCloseReceipt, MintError> {
        let stream_id = StreamId::parse(&req.stream_id)?;
        let hash = op_hash(&json!({
            "op": "stream.close",
            "stream_id": stream_id.as_str(),
            "units": req.units,
            "seq": req.seq,
            "idem_key": req.idem_key,
            "ref": req.ref_,
        }));
        let out = self
            .store
            .commit_stream_close(StreamCloseCommit {
                stream_id,
                units: req.units,
                seq: req.seq,
                idem_key: req.idem_key,
                op_hash: hash,
                ref_: req.ref_,
            })
            .await?;
        Ok(StreamCloseReceipt {
            stream_id: out.stream_id,
            settled_total_micros: out.settled_total_micros,
            released_micros: out.released_micros,
            from_balance: out.from_balance,
            to_balance: out.to_balance,
            idempotent_replay: out.idempotent_replay,
        })
    }

    /// Read a live stream record.
    pub async fn read_stream(&self, id: &StreamId) -> Result<StreamRecord, MintError> {
        self.store.get_stream(id).await
    }
}
