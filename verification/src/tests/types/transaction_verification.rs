use alloc::{format, vec, vec::Vec};
use std::fs::read_to_string;

use eth2_types::{BeaconBlock, BeaconBlockHeader, MainnetEthSpec};
use eth_light_client_in_ckb_prover::{CachedBeaconBlock, Receipts};
use ethers_core::types::TransactionReceipt;
use tree_hash::TreeHash as _;

use super::load_beacon_block_header_from_json_or_create_default;
use crate::{
    mmr,
    tests::{find_json_file, find_json_files, setup},
    types::{core, packed, prelude::*},
};

#[test]
fn test_transaction_verification_case_1() {
    test_transaction_verification(1);
}

#[test]
fn test_transaction_verification_case_2() {
    test_transaction_verification(2);
}

#[test]
fn test_transaction_verification_case_3() {
    test_transaction_verification(3);
}

#[test]
fn test_transaction_verification_case_4() {
    test_transaction_verification(4);
}

#[test]
fn test_transaction_verification_case_5() {
    test_transaction_verification(5);
}

fn test_transaction_verification(case_id: usize) {
    setup();

    let beacon_dir = format!("mainnet/case-{}/beacon", case_id);
    let execution_dir = format!("mainnet/case-{}/execution", case_id);

    let header_json_files = find_json_files(&beacon_dir, "block-header-slot-");
    let block_json_files = find_json_files(&beacon_dir, "block-slot-");

    let headers = header_json_files
        .into_iter()
        .map(load_beacon_block_header_from_json_or_create_default)
        .collect::<Vec<BeaconBlockHeader>>();

    let blocks = block_json_files
        .into_iter()
        .filter_map(|file| {
            let json_str = read_to_string(file).unwrap();
            let json_value: serde_json::Value = serde_json::from_str(&json_str).unwrap();
            if json_value.get("code").is_some() {
                None
            } else {
                let block: BeaconBlock<MainnetEthSpec> =
                    serde_json::from_value(json_value["data"]["message"].clone()).unwrap();
                Some(block.into())
            }
        })
        .collect::<Vec<CachedBeaconBlock<MainnetEthSpec>>>();

    let store = mmr::lib::util::MemStore::default();
    let mmr = {
        let mut mmr = mmr::ClientRootMMR::new(0, &store);
        for header in &headers {
            let header: core::Header = packed::Header::from_ssz_header(header).unpack();
            mmr.push(header.calc_cache().digest()).unwrap();
        }
        mmr
    };

    let last_header = &headers[headers.len() - 1];
    let client = core::Client {
        id: 0,
        minimal_slot: headers[0].slot.into(),
        maximal_slot: last_header.slot.into(),
        tip_valid_header_root: last_header.tree_hash_root(),
        headers_mmr_root: mmr.get_root().unwrap().unpack(),
    };

    for (header, block) in headers
        .into_iter()
        .filter(|ref header| {
            let header: core::Header = packed::Header::from_ssz_header(header).unpack();
            !header.is_empty()
        })
        .zip(blocks.into_iter())
    {
        let slot = block.slot();
        let number = block.number();
        assert_eq!(
            slot, header.slot,
            "failed to check the slot for beacon block and its header:\
            block is #{} (number: {}) but header is #{}",
            slot, number, header.slot,
        );

        let receipts: Receipts = {
            let json_filename = format!("block-receipts-number-{number}.json");
            let json_file = find_json_file(&execution_dir, &json_filename);
            let json_str = read_to_string(&json_file).unwrap_or_else(|err| {
                panic!("failed to read file {} since {}", json_file.display(), err)
            });
            let receipts: Vec<TransactionReceipt> = serde_json::from_str(&json_str).unwrap();
            receipts.into()
        };

        let transactions_count = block.transactions_count();
        let receipts_count = receipts.original().len();
        assert_eq!(
            transactions_count, receipts_count,
            "failed to check the receipts size for block#{}: expect {} but actual {}",
            slot, transactions_count, receipts_count,
        );

        let header: core::Header = packed::Header::from_ssz_header(&header).unpack();

        let receipts_root = receipts.root();
        let receipts_root_ssz_proof = block.generate_receipts_root_proof_for_block_body();

        let index = slot - client.minimal_slot;
        let position = mmr::lib::leaf_index_to_pos(index.into());

        let header_mmr_proof = mmr
            .gen_proof(vec![position])
            .unwrap()
            .proof_items()
            .iter()
            .map(|item| item.unpack())
            .collect::<Vec<_>>();

        for index in 0..receipts_count {
            let transaction_ssz_proof = block.generate_transaction_proof_for_block_body(index);
            let receipt_mpt_proof = receipts.generate_proof(index);

            let proof = core::TransactionProof {
                header: header.clone(),
                transaction_index: index as u64,
                receipts_root: receipts_root,
                header_mmr_proof: header_mmr_proof.clone(),
                transaction_ssz_proof,
                receipt_mpt_proof,
                receipts_root_ssz_proof: receipts_root_ssz_proof.clone(),
            };

            let payload = core::TransactionPayload {
                transaction: block.transaction(index).unwrap().to_vec(),
                receipt: receipts.encode_data(index),
            };

            let packed_proof = proof.pack();
            let packed_payload = payload.pack();

            let result = client.verify_packed_transaction_proof(packed_proof.as_reader());
            assert!(
                result.is_ok(),
                "failed to verify packed proof for block#{}.transaction#{}",
                number,
                index
            );

            let result = proof.verify_packed_payload(packed_payload.as_reader());
            assert!(
                result.is_ok(),
                "failed to verify packed payload for block#{}.transaction#{}",
                number,
                index
            );
        }
    }
}
