mod solana_manager;
pub use solana_manager::*;

// #[async_trait::async_trait]
// pub trait TxRequester {
//     async fn request_send_transaction(
//         &self,
//         tx_request: TransactionRequest,
//         metadata: TxRequestMetadata,
//         sub_log_target: &str,
//     ) {
//         match self.tx_request_sender().send(TxRequestMessage::new(
//             TxRequest::Legacy(tx_request),
//             metadata.clone(),
//             true,
//             false,
//             GasCoefficient::Mid,
//             false,
//         )) {
//             Ok(_) => log::info!(
//                 target: LOG_TARGET,
//                 "-[{}] 🔖 Request relay transaction: {}",
//                 sub_display_format(sub_log_target),
//                 metadata
//             ),
//             Err(error) => {
//                 log::error!(
//                     target: LOG_TARGET,
//                     "-[{}] ❗️ Failed to send relay transaction: {}, Error: {}",
//                     sub_display_format(sub_log_target),
//                     metadata,
//                     error.to_string()
//                 );
//                 sentry::capture_message(
//                     format!(
//                         "[{}]-[{}]-[{}] ❗️ Failed to send relay transaction: {}, Error: {}",
//                         LOG_TARGET,
//                         sub_log_target,
//                         self.bfc_client().address(),
//                         metadata,
//                         error
//                     )
//                     .as_str(),
//                     sentry::Level::Error,
//                 );
//             }
//         }
//     }
// }
