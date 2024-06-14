// use solana_rpc_client::rpc_client::RpcClient;
use solana_client::rpc_client::RpcClient;
use solana_sdk::message::Message;
use solana_sdk::pubkey::Pubkey;
use sp_core::H256;
use std::sync::Arc;

use tokio::{
	sync::{
		broadcast,
		broadcast::{Receiver, Sender},
	},
	time::{sleep, Duration},
};
use tokio_stream::StreamExt;

const SUB_LOG_TARGET: &str = "event-manager";

/// A full Hyperlane message between chains
#[derive(Clone, Eq, PartialEq, Hash)]
pub struct SolanaMessage {
	/// Message nonce
	pub nonce: u32,
	/// Origin domain ID
	pub origin: u32,
	/// Address in origin convention
	pub sender: H256,
	/// Destination domain ID
	pub destination: u32,
	/// Address in destination convention
	pub recipient: H256,
}

#[derive(Debug, Clone, Eq, PartialEq)]
/// A Bitcoin related event type.
pub enum EventType {
	/// An inbound action.
	Inbound,
	/// An outbound action.
	Outbound,
}

#[derive(Clone, Debug)]
/// The message format passed through the slot channel.
pub struct EventMessage {
	/// The processed slot number.
	pub slot_number: u64,
	/// The detected transaction logs from the target contracts.
	pub event_messages: Vec<Message>,
}

impl EventMessage {
	pub fn new(slot_number: u64, event_messages: Vec<Message>) -> Self {
		Self { slot_number, event_messages }
	}
}

/// The essential task that listens and handle new events.
pub struct EventManager {
	/// The solana client for the connected chain.
	pub sol_client: Arc<RpcClient>,
	/// The block waiting for enough confirmations.
	waiting_block: u64,
	/// The configured minimum block confirmations required to process a block.
	block_confirmations: u64,
	program_id: Pubkey,
}

impl EventManager {
	/// Instantiates a new `EventManager` instance.
	pub fn new(sol_client: Arc<RpcClient>, program_id: Pubkey) -> Self {
		let (sender, _receiver): (Sender<()>, Receiver<()>) = broadcast::channel(512);

		Self { sol_client, waiting_block: u64::default(), block_confirmations: 0, program_id }
	}

	/// Initialize event manager.
	async fn initialize(&mut self) {
		// 최소 잔액 확인
		self.sol_client.get_minimum_balance_for_rent_exemption(0);

		// 최신 슬롯을 waiting_block으로 설정
		self.waiting_block = self.sol_client.get_slot().unwrap();
	}

	/// Starts the event manager. Reads every new mined block of the connected chain and starts to
	/// publish to the event channel.
	pub async fn run(&mut self) {
		self.initialize().await;

		loop {
			let latest_block = self.sol_client.get_slot().unwrap();
			while self.is_block_confirmed(latest_block) {
				self.process_confirmed_block().await;
			}

			sleep(Duration::from_millis(500)).await;
		}
	}

	/// Process the confirmed block and verifies if any events emitted from the target
	/// contracts.
	#[inline]
	async fn process_confirmed_block(&mut self) {
		let from_block = self.waiting_block;
		let to_block = self.sol_client.get_slot().unwrap();

		let blocks = self.sol_client.get_blocks(from_block, Some(to_block)).unwrap();

		for block in blocks {
			let encoded_confirmed_block = self.sol_client.get_block(block).unwrap();

			let block_data = encoded_confirmed_block.transactions;

			for transaction in block_data {
				let decoded_transaction = transaction.transaction.decode().unwrap(); // 이 부분은 실제 API와 다를 수 있습니다.

				for instruction in decoded_transaction.message.instructions() {
					let program_id =
						instruction.program_id(&decoded_transaction.message.static_account_keys());

					if program_id == &self.program_id {
						// 프로그램과 상호작용하는 명령 처리
						println!(
							"Found instruction interacting with the program: {:?}",
							instruction
						);
					}
				}
			}
		}
	}
	/// Verifies if the stored waiting block has waited enough.
	#[inline]
	fn is_block_confirmed(&self, latest_block_num: u64) -> bool {
		latest_block_num.saturating_sub(self.waiting_block) >= self.block_confirmations
	}
}
