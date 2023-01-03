pub mod core;
mod generated;
pub mod prelude;

mod conversion;

use ::core::result::Result;
use alloc::{vec, vec::Vec};

use ckb_mmr::{leaf_index_to_mmr_size, leaf_index_to_pos, Error as MMRError};
use rlp::encode;
use ssz_types::{typenum, VariableList};
use tree_hash::TreeHash as _;

pub use generated::packed;

use self::prelude::*;
use crate::{
    constants::generalized_index_offsets,
    error::{ProofUpdateError, TxVerificationError},
    mmr, ssz, trie,
};

impl core::Client {
    pub fn new_from_packed_proof_update(
        packed_proof_update: packed::ProofUpdateReader,
    ) -> Result<Self, ProofUpdateError> {
        let updates = packed_proof_update.updates();

        // At least, there should has 1 new header.
        if updates.is_empty() {
            return Err(ProofUpdateError::EmptyUpdates);
        }

        let updates_len = updates.len();
        let mut updates_iter = updates.iter();

        let mut cached_finalized_headers = Vec::with_capacity(updates_len);
        let mut prev_cached_header: mmr::HeaderWithCache;
        let mut curr_header: core::Header;
        let mut digests_with_positions = Vec::with_capacity(updates_len);

        curr_header = updates_iter.next().unwrap().finalized_header().unpack();

        let minimal_slot = curr_header.slot;

        let mut header_mmr_index = 0;

        // Check Updates
        {
            // Check if updates are continuous
            for update in updates_iter {
                prev_cached_header = curr_header.calc_cache();
                curr_header = update.finalized_header().unpack();

                if prev_cached_header.inner.slot + 1 != curr_header.slot {
                    return Err(ProofUpdateError::UncontinuousSlot);
                }

                if prev_cached_header.root != curr_header.parent_root {
                    return Err(ProofUpdateError::UnmatchedParentRoot);
                }

                // TODO verify more, such as BLS

                let position = leaf_index_to_pos(header_mmr_index);
                let digest = prev_cached_header.digest();

                cached_finalized_headers.push(prev_cached_header);
                digests_with_positions.push((position, digest));

                header_mmr_index += 1;
            }
        }

        let maximal_slot = curr_header.slot;

        // Handle the last update
        {
            prev_cached_header = curr_header.calc_cache();
            let position = leaf_index_to_pos(header_mmr_index);
            let digest = prev_cached_header.digest();
            cached_finalized_headers.push(prev_cached_header);
            digests_with_positions.push((position, digest));
        }

        // Check New MMR Root
        {
            let proof: mmr::MMRProof = {
                let max_index = maximal_slot - minimal_slot;
                let mmr_size = leaf_index_to_mmr_size(max_index);
                let proof = packed_proof_update
                    .new_headers_mmr_proof()
                    .iter()
                    .map(|r| r.to_entity())
                    .collect::<Vec<_>>();
                mmr::MMRProof::new(mmr_size, proof)
            };
            let result = proof
                .verify(
                    packed_proof_update.new_headers_mmr_root().to_entity(),
                    digests_with_positions,
                )
                .map_err(|_| ProofUpdateError::Other)?;
            if !result {
                return Err(ProofUpdateError::HeadersMmrProof);
            }
        }

        let tip_header_root = cached_finalized_headers[updates_len - 1].root;
        let headers_mmr_root = packed_proof_update.new_headers_mmr_root().unpack();
        let new_client = Self {
            minimal_slot,
            maximal_slot,
            tip_header_root,
            headers_mmr_root,
        };

        Ok(new_client)
    }

    pub fn try_apply_packed_proof_update(
        &self,
        packed_proof_update: packed::ProofUpdateReader,
    ) -> Result<Self, ProofUpdateError> {
        let updates = packed_proof_update.updates();

        // At least, there should has 1 new header.
        if updates.is_empty() {
            return Err(ProofUpdateError::EmptyUpdates);
        }

        let updates_len = updates.len();
        let mut updates_iter = updates.iter();

        let mut cached_finalized_headers = Vec::with_capacity(updates_len);
        let mut prev_cached_header: mmr::HeaderWithCache;
        let mut curr_header: core::Header;
        let mut digests_with_positions = Vec::with_capacity(updates_len);
        let mut header_mmr_index = self.maximal_slot - self.minimal_slot;

        // Check Old Tip Header (the first header)
        {
            curr_header = updates_iter.next().unwrap().finalized_header().unpack();
            if curr_header.slot != self.maximal_slot + 1 {
                return Err(ProofUpdateError::FirstHeaderSlot);
            }
            if curr_header.parent_root != self.tip_header_root {
                return Err(ProofUpdateError::FirstHeaderParentRoot);
            }
        }

        // Check Updates
        {
            // Check if updates are continuous
            for update in updates_iter {
                header_mmr_index += 1;
                prev_cached_header = curr_header.calc_cache();
                curr_header = update.finalized_header().unpack();

                if prev_cached_header.inner.slot + 1 != curr_header.slot {
                    return Err(ProofUpdateError::UncontinuousSlot);
                }

                if prev_cached_header.root != curr_header.parent_root {
                    return Err(ProofUpdateError::UnmatchedParentRoot);
                }

                // TODO verify more, such as BLS

                let position = leaf_index_to_pos(header_mmr_index);
                let digest = prev_cached_header.digest();

                cached_finalized_headers.push(prev_cached_header);
                digests_with_positions.push((position, digest));
            }
        }

        let new_maximal_slot = curr_header.slot;

        // Handle the last update
        {
            header_mmr_index += 1;
            prev_cached_header = curr_header.calc_cache();
            let position = leaf_index_to_pos(header_mmr_index);
            let digest = prev_cached_header.digest();
            cached_finalized_headers.push(prev_cached_header);
            digests_with_positions.push((position, digest));
        }

        // Check New MMR Root
        {
            let proof: mmr::MMRProof = {
                let max_index = new_maximal_slot - self.minimal_slot;
                let mmr_size = leaf_index_to_mmr_size(max_index);
                let proof = packed_proof_update
                    .new_headers_mmr_proof()
                    .iter()
                    .map(|r| r.to_entity())
                    .collect::<Vec<_>>();
                mmr::MMRProof::new(mmr_size, proof)
            };
            let result = proof
                .verify(
                    packed_proof_update.new_headers_mmr_root().to_entity(),
                    digests_with_positions,
                )
                .map_err(|_| ProofUpdateError::Other)?;
            if !result {
                return Err(ProofUpdateError::HeadersMmrProof);
            }
        }

        let new_tip_header_root = cached_finalized_headers[updates_len - 1].root;
        let new_headers_mmr_root = packed_proof_update.new_headers_mmr_root().unpack();
        let new_client = Self {
            minimal_slot: self.minimal_slot,
            maximal_slot: new_maximal_slot,
            tip_header_root: new_tip_header_root,
            headers_mmr_root: new_headers_mmr_root,
        };

        Ok(new_client)
    }

    pub fn verify_packed_transaction_proof(
        &self,
        tx_proof: packed::TransactionProofReader,
    ) -> Result<(), TxVerificationError> {
        let header_slot = tx_proof.header().slot().unpack();
        if self.minimal_slot > header_slot || self.maximal_slot < header_slot {
            return Err(TxVerificationError::Unsynchronized);
        }
        let result = self
            .verify_single_header(tx_proof.header(), tx_proof.header_mmr_proof())
            .map_err(|_| TxVerificationError::Other)?;
        if !result {
            Err(TxVerificationError::HeaderMmrProof)
        } else {
            Ok(())
        }
    }

    fn verify_single_header(
        &self,
        header: packed::HeaderReader,
        header_mmr_proof: packed::MmrProofReader,
    ) -> Result<bool, MMRError> {
        let header_slot = header.slot().unpack();
        let proof: mmr::MMRProof = {
            let max_index = self.maximal_slot - self.minimal_slot;
            let mmr_size = leaf_index_to_mmr_size(max_index);
            let proof = header_mmr_proof
                .iter()
                .map(|r| r.to_entity())
                .collect::<Vec<_>>();
            mmr::MMRProof::new(mmr_size, proof)
        };
        let digests_with_positions = {
            let index = header_slot - self.minimal_slot;
            let position = leaf_index_to_pos(index);
            let digest = header.unpack().calc_cache().digest();
            vec![(position, digest)]
        };
        proof.verify(self.headers_mmr_root.pack(), digests_with_positions)
    }
}

impl core::TransactionProof {
    pub fn verify_packed_payload(
        &self,
        payload: packed::TransactionPayloadReader,
    ) -> Result<(), TxVerificationError> {
        self.verify_transaction(payload.transaction().raw_data())?;
        self.verify_receipt(payload.receipt().raw_data())
    }

    pub fn verify_transaction(&self, transaction: &[u8]) -> Result<(), TxVerificationError> {
        // Since `MAX_BYTES_PER_TRANSACTION`.
        VariableList::<u8, typenum::U1073741824>::new(transaction.to_vec())
            .map_err(|_| TxVerificationError::Other)
            .and_then(|tx| {
                if !ssz::verify_merkle_proof(
                    self.header.body_root,
                    tx.tree_hash_root(),
                    &self.transaction_ssz_proof,
                    self.transaction_index as usize
                        + generalized_index_offsets::TRANSACTION_IN_BLOCK_BODY,
                ) {
                    Err(TxVerificationError::TransactionSszProof)
                } else {
                    Ok(())
                }
            })
    }

    pub fn verify_receipt(&self, receipt: &[u8]) -> Result<(), TxVerificationError> {
        let key = encode(&self.transaction_index);
        if !trie::verify_proof(
            &self.receipt_mpt_proof,
            self.receipts_root.as_bytes(),
            &key,
            receipt,
        ) {
            Err(TxVerificationError::ReceiptMptProof)
        } else if !ssz::verify_merkle_proof(
            self.header.body_root,
            self.receipts_root,
            &self.receipts_root_ssz_proof,
            generalized_index_offsets::RECEIPTS_ROOT_IN_BLOCK_BODY,
        ) {
            Err(TxVerificationError::ReceiptsRootSszProof)
        } else {
            Ok(())
        }
    }
}
