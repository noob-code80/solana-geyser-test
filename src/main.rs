use anyhow::Result;
use futures::{SinkExt, StreamExt};
use log::{error, info};
use std::collections::HashMap;
use tokio;
use bs58;
use yellowstone_grpc_client::{GeyserGrpcClient, ClientTlsConfig};
use yellowstone_grpc_proto::prelude::{
    CommitmentLevel, SubscribeRequest, SubscribeRequestFilterTransactions,
    SubscribeUpdate, subscribe_update::UpdateOneof,
};

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    
    // Инициализируем криптопровайдер для rustls
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Failed to install crypto provider");

    let endpoint = "http://fr.grpc.gadflynode.com:25565";

    let mut client = GeyserGrpcClient::build_from_shared(endpoint.to_string())?
        .tls_config(ClientTlsConfig::new().with_native_roots())?
        .connect()
        .await?;

    let mut transactions_filters: HashMap<String, SubscribeRequestFilterTransactions> = HashMap::new();
    transactions_filters.insert(
        "all_transactions".to_string(),
        SubscribeRequestFilterTransactions {
            vote: Some(false),
            failed: None,
            signature: None,
            account_include: vec![],
            account_exclude: vec![],
            account_required: vec![],
        },
    );

    let (mut subscribe_tx, mut updates_stream) = client.subscribe().await?;

    let request = SubscribeRequest {
        transactions: transactions_filters,
        commitment: Some(CommitmentLevel::Confirmed as i32),
        ..Default::default()
    };

    subscribe_tx.send(request).await?;
    info!("✅ Подписка отправлена");

    while let Some(message) = updates_stream.next().await {
        match message {
            Ok(update) => process_update(update),
            Err(e) => {
                error!("Ошибка стрима: {:?}", e);
                break;
            }
        }
    }

    Ok(())
}

fn process_update(update: SubscribeUpdate) {
    if let Some(update_oneof) = update.update_oneof {
        match update_oneof {
            UpdateOneof::Transaction(tx_info) => {
                info!("Транзакция в слоте {}", tx_info.slot);

                if let Some(tx) = &tx_info.transaction {
                    // Получаем подпись из transaction
                    let signature = if !tx.signature.is_empty() {
                        bs58::encode(&tx.signature).into_string()
                    } else if let Some(tx_data) = &tx.transaction {
                        if let Some(first_sig) = tx_data.signatures.first() {
                            bs58::encode(first_sig).into_string()
                        } else {
                            "unknown".to_string()
                        }
                    } else {
                        "unknown".to_string()
                    };
                    info!("Подпись: {}", signature);

                    if let Some(tx_data) = &tx.transaction {
                        if let Some(message) = &tx_data.message {
                            info!("Аккаунты: {:?}", message.account_keys.iter().map(|key| bs58::encode(key).into_string()).collect::<Vec<_>>());
                            info!("Recent blockhash: {}", bs58::encode(&message.recent_blockhash).into_string());

                            for instr in &message.instructions {
                                let program_id = bs58::encode(&message.account_keys[instr.program_id_index as usize]).into_string();
                                info!("Инструкция: Программа {}, Аккаунты: {:?}, Data: {:?}",
                                    program_id,
                                    instr.accounts.iter().map(|&idx| bs58::encode(&message.account_keys[idx as usize]).into_string()).collect::<Vec<_>>(),
                                    instr.data
                                );
                            }
                        }
                    }

                    if let Some(meta) = &tx.meta {
                        if meta.err.is_some() {
                            info!("Ошибка tx: {:?}", meta.err);
                        }
                        info!("Fee: {}", meta.fee);
                    }
                }
            }
            _ => {}
        }
    }
}