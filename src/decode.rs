use anyhow::{anyhow, Result as AnyResult};
use dialoguer::Confirm;
use indexmap::IndexMap;
use log::{debug, error, info};
use metaboss_lib::decode::decode_master_edition_from_mint;
use mpl_token_metadata::state::{Key, Metadata, TokenStandard, UseMethod};
use retry::{delay::Exponential, retry};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use solana_client::{nonblocking::rpc_client::RpcClient as AsyncRpcClient, rpc_client::RpcClient};
use solana_program::borsh::try_from_slice_unchecked;
use solana_sdk::pubkey::Pubkey;
use std::cmp;
use std::str::FromStr;
use std::sync::Arc;
use std::{fs::File, io::Write};
use tokio::task::JoinHandle;

use crate::{constants::*, errors::*, parse::is_only_one_option, spinner};
// use crate::limiter::create_rate_limiter;

pub const OPEN_FILES_LIMIT: usize = 100;

#[derive(Debug, Serialize)]
pub struct JSONCreator {
    pub address: String,
    pub verified: bool,
    pub share: u8,
}

#[derive(Debug, Serialize)]
pub struct JSONCollection {
    pub verified: bool,
    pub key: String,
}

#[derive(Debug, Serialize)]
pub struct JSONUses {
    pub use_method: String,
    pub remaining: u64,
    pub total: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct DecodeCache(IndexMap<String, CacheItem>);
pub type DecodeResults = Vec<Result<Metadata, DecodeError>>;

impl Default for DecodeCache {
    fn default() -> Self {
        Self::new()
    }
}

impl DecodeCache {
    pub fn new() -> Self {
        DecodeCache(IndexMap::new())
    }

    pub fn write<W: Write>(self, writer: W) -> AnyResult<()> {
        serde_json::to_writer(writer, &self)?;
        Ok(())
    }

    pub fn write_errors<W: Write>(self, writer: W) -> AnyResult<()> {
        let errors = self
            .0
            .iter()
            .filter(|(_, v)| !v.successful)
            .collect::<IndexMap<_, _>>();
        serde_json::to_writer(writer, &errors)?;
        Ok(())
    }

    pub fn update_errors(
        &mut self,
        errors: Vec<Result<mpl_token_metadata::state::Metadata, DecodeError>>,
    ) {
        let errors = errors.iter().map(|r| r.as_ref()).map(Result::unwrap_err);

        // This is really verbose; surely there's a better way.
        for error in errors {
            match error {
                DecodeError::MissingAccount(mint_address) => {
                    *self.0.get_mut(mint_address).unwrap() = CacheItem {
                        successful: false,
                        error: Some(error.to_string()),
                    };
                }
                DecodeError::ClientError(mint_address, _client_error_kind) => {
                    *self.0.get_mut(mint_address).unwrap() = CacheItem {
                        successful: false,
                        error: Some(error.to_string()),
                    };
                }
                DecodeError::NetworkError(mint_address, _network_error) => {
                    *self.0.get_mut(mint_address).unwrap() = CacheItem {
                        successful: false,
                        error: Some(error.to_string()),
                    };
                }
                DecodeError::PubkeyParseFailed(mint_address) => {
                    *self.0.get_mut(mint_address).unwrap() = CacheItem {
                        successful: false,
                        error: Some(error.to_string()),
                    };
                }
                DecodeError::DecodeMetadataFailed(mint_address, _decode_error) => {
                    *self.0.get_mut(mint_address).unwrap() = CacheItem {
                        successful: false,
                        error: Some(error.to_string()),
                    };
                }
            }
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CacheItem {
    pub successful: bool,
    pub error: Option<String>,
}

pub async fn decode_metadata_all(
    client: Arc<AsyncRpcClient>,
    json_file: &str,
    full: bool,
    output: String,
) -> AnyResult<()> {
    let file = File::open(json_file)?;
    let mut mint_accounts: Vec<String> = serde_json::from_reader(file)?;
    // let use_rate_limit = *USE_RATE_LIMIT.read().unwrap();
    // let handle = create_rate_limiter();

    let cache_file_name = "metaboss-cache-decode.json";

    // Create a temp cache file to track failures.
    let f = File::create(cache_file_name)?;

    // Mint addresses success starts out as false and we will update on success.
    let mut cache = DecodeCache::new();
    for mint in &mint_accounts {
        cache.0.insert(
            mint.clone(),
            CacheItem {
                successful: false,
                error: None,
            },
        );
    }

    // Loop over decode process so we can retry repeatedly until the user exits.
    loop {
        let remaining_mints = mint_accounts.clone();

        info!("Sending network requests...");
        let spinner = spinner::create_spinner("Sending network requests....");
        // Create a vector of futures to execute.
        let decode_tasks: Vec<_> = remaining_mints
            .into_iter()
            .map(|mint_account| tokio::spawn(async_decode(client.clone(), mint_account)))
            .collect();
        spinner.finish();

        let decode_tasks_len = decode_tasks.len();

        // Wait for all the tasks to resolve and push the results to our results vector
        let mut metadata_results = Vec::new();
        let spinner = spinner::create_spinner("Awaiting results...");
        for task in decode_tasks {
            metadata_results.push(task.await.unwrap());
        }
        spinner.finish();

        // Partition decode results.
        let (decode_successful, decode_failed): (DecodeResults, DecodeResults) =
            metadata_results.into_iter().partition(Result::is_ok);

        // Unwrap sucessful
        let mut decode_successful: Vec<Metadata> =
            decode_successful.into_iter().map(Result::unwrap).collect();

        // Mark successful ones in the cache.
        for md in &decode_successful {
            (*cache.0.get_mut(&md.mint.to_string()).unwrap()).successful = true;
        }

        // Take all the successful ones, unwrap them and then write them to files, consuming them.
        let spinner = spinner::create_spinner("Writing to files...");

        let mut write_results = Vec::new();

        while !decode_successful.is_empty() {
            let write_tasks: Vec<JoinHandle<Result<(), anyhow::Error>>> = decode_successful
                .drain(0..cmp::min(decode_successful.len(), OPEN_FILES_LIMIT))
                .into_iter()
                .map(|md| tokio::spawn(write_metadata_to_file(md, full, output.clone())))
                .collect();

            // Wait for all write tasks to resolve.
            for task in write_tasks {
                write_results.push(task.await.unwrap());
            }
        }

        spinner.finish();

        // Partition write results.
        let (_write_successful, _write_failed): (Vec<AnyResult<()>>, Vec<AnyResult<()>>) =
            write_results.into_iter().partition(Result::is_ok);

        // If some of the decodes failed, ask user if they wish to retry and the loop starts again.
        // Otherwise, break out of the loop and write the cache to disk.
        if !decode_failed.is_empty() {
            let msg = format!(
                "{}/{} decodes failed. Do you want to retry these ones?",
                &decode_failed.len(),
                decode_tasks_len
            );
            if Confirm::new().with_prompt(msg).interact()? {
                mint_accounts = cache
                    .0
                    .keys()
                    .filter(|&k| !cache.0[k].successful)
                    .map(|m| m.to_string())
                    .collect();
                continue;
            } else {
                // We have failures but the user has decided to stop so we log failures to the cache file.
                cache.update_errors(decode_failed);

                println!("Writing cache to file...{}", cache_file_name);
                cache.write_errors(f)?;
                break;
            }
        } else {
            // None failed so we exit the loop.
            println!("All decodes successful!");
            break;
        }
    }

    Ok(())
}

async fn write_metadata_to_file(metadata: Metadata, full: bool, output: String) -> AnyResult<()> {
    let mint_address = metadata.mint;
    debug!(
        "Converting metadata into JSON for mint account {}",
        mint_address
    );
    let json_metadata = match decode_to_json(metadata, full) {
        Ok(j) => j,
        Err(err) => {
            error!(
                "Failed to decode metadata to JSON for mint account: {}, error: {}",
                mint_address, err
            );
            return Err(anyhow!(
                "Failed to decode metadata to JSON for mint account: {}",
                mint_address
            ));
        }
    };

    debug!("Creating file for mint account: {}", mint_address);
    let mut file = match File::create(format!("{}/{}.json", output, mint_address)) {
        Ok(f) => f,
        Err(err) => {
            error!(
                "Failed to create JSON file for mint account: {}, error: {}",
                mint_address, err
            );
            return Err(anyhow!(
                "Failed to create JSON file for mint account: {}, error: {}",
                mint_address,
                err
            ));
        }
    };

    debug!("Writing to file for mint account: {}", mint_address);
    match serde_json::to_writer(&mut file, &json_metadata) {
        Ok(_) => (),
        Err(err) => {
            error!(
                "Failed to write JSON file for mint account: {}, error: {}",
                mint_address, err
            );
            return Err(anyhow!(
                "Failed to write JSON file for mint account: {}, error: {}",
                mint_address,
                err
            ));
        }
    }

    Ok(())
}

pub fn decode_master_edition(client: &RpcClient, mint_account: &str) -> AnyResult<()> {
    let master_edition = decode_master_edition_from_mint(client, mint_account)?;
    println!("{:?}", master_edition);

    Ok(())
}

pub async fn decode_from_metadata(
    client: &AsyncRpcClient,
    metadata_address: &str,
) -> AnyResult<()> {
    let metadata_pubkey = Pubkey::from_str(metadata_address)?;

    let account_data = client
        .get_account_data(&metadata_pubkey)
        .await
        .map_err(|err| DecodeError::NetworkError(metadata_address.to_string(), err.to_string()))?;

    let metadata: Metadata = match try_from_slice_unchecked(&account_data) {
        Ok(m) => m,
        Err(err) => {
            return Err(anyhow!(DecodeError::DecodeMetadataFailed(
                metadata_address.to_string(),
                err.to_string(),
            )))
        }
    };

    println!("{:?}", metadata);

    Ok(())
}

pub async fn decode_metadata(
    client: AsyncRpcClient,
    account: Option<&String>,
    full: bool,
    list_path: Option<&String>,
    raw: bool,
    output: String,
) -> AnyResult<()> {
    // Explicitly warn the user if they provide incorrect options combinations
    if !is_only_one_option(&account, &list_path) {
        return Err(anyhow!(
            "Please specify either a mint account or a list of mint accounts, but not both."
        ));
    }

    let client = Arc::new(client);

    if let Some(mint_account) = account {
        if raw {
            let data = async_decode_raw(client, mint_account).await?;
            println!("{:?}", data);
            return Ok(());
        }
        let metadata = async_decode(client, mint_account.to_string()).await?;
        let json_metadata = decode_to_json(metadata, full)?;
        let mut file = File::create(format!("{}/{}.json", output, mint_account))?;
        serde_json::to_writer(&mut file, &json_metadata)?;
    } else if let Some(list_path) = list_path {
        decode_metadata_all(client, list_path, full, output).await?;
    } else {
        return Err(anyhow!(
            "Please specify either a mint account or a list of mint accounts, but not both."
        ));
    };

    Ok(())
}

pub async fn async_decode_raw(
    client: Arc<AsyncRpcClient>,
    mint_account: &str,
) -> Result<Vec<u8>, DecodeError> {
    let pubkey = match Pubkey::from_str(mint_account) {
        Ok(pubkey) => pubkey,
        Err(_) => return Err(DecodeError::PubkeyParseFailed(mint_account.to_string())),
    };
    let metadata_pda = get_metadata_pda(pubkey);

    let account_data = client
        .get_account_data(&metadata_pda)
        .await
        .map_err(|err| DecodeError::NetworkError(mint_account.to_string(), err.to_string()))?;
    Ok(account_data)
}

pub fn decode_raw(client: &RpcClient, mint_account: &str) -> Result<Vec<u8>, DecodeError> {
    let pubkey = match Pubkey::from_str(mint_account) {
        Ok(pubkey) => pubkey,
        Err(_) => return Err(DecodeError::PubkeyParseFailed(mint_account.to_string())),
    };
    let metadata_pda = get_metadata_pda(pubkey);

    let account_data = match retry(
        Exponential::from_millis_with_factor(250, 2.0).take(3),
        || client.get_account_data(&metadata_pda),
    ) {
        Ok(data) => data,
        Err(err) => {
            return Err(DecodeError::NetworkError(
                mint_account.to_string(),
                err.to_string(),
            ));
        }
    };
    Ok(account_data)
}

pub async fn async_decode(
    client: Arc<AsyncRpcClient>,
    mint_account: String,
) -> Result<Metadata, DecodeError> {
    let pubkey = match Pubkey::from_str(&mint_account) {
        Ok(pubkey) => pubkey,
        Err(_) => return Err(DecodeError::PubkeyParseFailed(mint_account.to_string())),
    };
    let metadata_pda = get_metadata_pda(pubkey);

    let account_data = client
        .get_account_data(&metadata_pda)
        .await
        .map_err(|err| DecodeError::NetworkError(mint_account.to_string(), err.to_string()))?;

    let metadata: Metadata = match try_from_slice_unchecked(&account_data) {
        Ok(m) => m,
        Err(err) => {
            return Err(DecodeError::DecodeMetadataFailed(
                mint_account.to_string(),
                err.to_string(),
            ))
        }
    };

    Ok(metadata)
}

pub fn decode(client: &RpcClient, mint_account: &str) -> Result<Metadata, DecodeError> {
    let pubkey = match Pubkey::from_str(mint_account) {
        Ok(pubkey) => pubkey,
        Err(_) => return Err(DecodeError::PubkeyParseFailed(mint_account.to_string())),
    };
    let metadata_pda = get_metadata_pda(pubkey);

    let account_data = client
        .get_account_data(&metadata_pda)
        .map_err(|err| DecodeError::NetworkError(mint_account.to_string(), err.to_string()))?;

    let metadata: Metadata = match try_from_slice_unchecked(&account_data) {
        Ok(m) => m,
        Err(err) => {
            return Err(DecodeError::DecodeMetadataFailed(
                mint_account.to_string(),
                err.to_string(),
            ))
        }
    };

    Ok(metadata)
}

fn decode_to_json(metadata: Metadata, full: bool) -> AnyResult<Value> {
    let mut creators: Vec<JSONCreator> = Vec::new();

    if let Some(c) = metadata.data.creators {
        creators = c
            .iter()
            .map(|c| JSONCreator {
                address: c.address.to_string(),
                verified: c.verified,
                share: c.share,
            })
            .collect::<Vec<JSONCreator>>();
    }

    let data_json = json!({
        "name": metadata.data.name.trim_matches(char::from(0)),
        "symbol": metadata.data.symbol.trim_matches(char::from(0)),
        "seller_fee_basis_points": metadata.data.seller_fee_basis_points,
        "uri": metadata.data.uri.trim_matches(char::from(0)),
        "creators": creators,
    });

    if !full {
        return Ok(data_json);
    }

    let mut token_standard: Option<String> = None;
    if let Some(ts) = metadata.token_standard {
        token_standard = Some(parse_token_standard(ts))
    }

    let mut collection: Option<JSONCollection> = None;
    if let Some(c) = metadata.collection {
        collection = Some(JSONCollection {
            verified: c.verified,
            key: c.key.to_string(),
        })
    }

    let mut uses: Option<JSONUses> = None;
    if let Some(u) = metadata.uses {
        uses = Some(JSONUses {
            use_method: parse_use_method(u.use_method),
            remaining: u.remaining,
            total: u.total,
        })
    }

    let json_metadata = json!({
        "key": parse_key(metadata.key),
        "update_authority": metadata.update_authority.to_string(),
        "mint_account": metadata.mint.to_string(),
        "nft_data": data_json,
        "primary_sale_happened": metadata.primary_sale_happened,
        "is_mutable": metadata.is_mutable,
        "edition_nonce": metadata.edition_nonce,
        "token_standard": token_standard,
        "collection": collection,
        "uses": uses,
    });
    Ok(json_metadata)
}

pub fn get_metadata_pda(pubkey: Pubkey) -> Pubkey {
    let metaplex_pubkey = METAPLEX_PROGRAM_ID
        .parse::<Pubkey>()
        .expect("Failed to parse Metaplex Program Id");

    let seeds = &[
        "metadata".as_bytes(),
        metaplex_pubkey.as_ref(),
        pubkey.as_ref(),
    ];

    let (pda, _) = Pubkey::find_program_address(seeds, &metaplex_pubkey);
    pda
}

fn parse_key(key: Key) -> String {
    match key {
        Key::Uninitialized => String::from("Uninitialized"),
        Key::EditionV1 => String::from("EditionV1"),
        Key::MasterEditionV1 => String::from("MasterEditionV1"),
        Key::ReservationListV1 => String::from("ReservationListV1"),
        Key::MetadataV1 => String::from("MetadataV1"),
        Key::ReservationListV2 => String::from("ReservationListV2"),
        Key::MasterEditionV2 => String::from("MasterEditionV2"),
        Key::EditionMarker => String::from("EditionMarker"),
        Key::UseAuthorityRecord => String::from("UseAuthorityRecord"),
        Key::CollectionAuthorityRecord => String::from("CollectionAuthorityRecord"),
    }
}

fn parse_token_standard(token_standard: TokenStandard) -> String {
    match token_standard {
        TokenStandard::NonFungible => String::from("NonFungible"),
        TokenStandard::FungibleAsset => String::from("FungibleAsset"),
        TokenStandard::Fungible => String::from("Fungible"),
        TokenStandard::NonFungibleEdition => String::from("NonFungibleEdition"),
    }
}

fn parse_use_method(use_method: UseMethod) -> String {
    match use_method {
        UseMethod::Burn => String::from("Burn"),
        UseMethod::Single => String::from("Single"),
        UseMethod::Multiple => String::from("Multiple"),
    }
}
