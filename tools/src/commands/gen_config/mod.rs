// Copyright 2021 Google LLC
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     https://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

mod cloudflare;

use crate::linux_commands::generate_private_key_pem;
use crate::runtime::openssl_signer::OpensslSigner;
use anyhow::{Error, Result};
use clap::Parser;
use cloudflare::CloudlareSpecificInput;
use serde::{Deserialize, Serialize};
use sxg_rs::acme::{directory::Directory as AcmeDirectory, Account as AcmeAccount};
use sxg_rs::crypto::EcPrivateKey;

#[derive(Debug, Parser)]
pub struct Opts {
    /// A YAML file containing all config values.
    /// You can use the template
    /// 'tools/src/commands/gen_config/input.example.yaml'.
    #[clap(long, value_name = "FILE_NAME")]
    input: String,
    /// A YAML file containing the generated values.
    #[clap(long, value_name = "FILE_NAME")]
    artifact: String,
    /// No longer log in to worker service providers.
    #[clap(long)]
    use_ci_mode: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    sxg_worker: sxg_rs::config::Config,
    certificates: SxgCertConfig,
    cloudflare: CloudlareSpecificInput,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SxgCertConfig {
    PreIssued {
        cert_file: String,
        issuer_file: String,
    },
    CreateAcmeAccount(AcmeConfig),
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AcmeConfig {
    server_url: String,
    contact_email: String,
    agreed_terms_of_service: String,
    sxg_cert_request_file: String,
    eab: Option<EabConfig>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct EabConfig {
    base64_mac_key: String,
    key_id: String,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Artifact {
    acme_account: Option<AcmeAccount>,
    acme_private_key_instruction: Option<String>,
    cloudflare_kv_namespace_id: Option<String>,
}

// Set working directory to the root folder of the "sxg-rs" repository.
fn goto_repository_root() -> Result<(), std::io::Error> {
    let exe_path = std::env::current_exe()?;
    assert!(exe_path.ends_with("target/debug/tools"));
    let repo_root = exe_path
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    std::env::set_current_dir(repo_root)?;
    Ok(())
}

fn read_certificate_pem_file(path: &str) -> Result<String> {
    let text = std::fs::read_to_string(path)
        .map_err(|_| Error::msg(format!(r#"Failed to read file "{}""#, path)))?;
    // Translate Windows-style line endings to Unix-style so the '\r' is
    // not rendered in the toml. This is purely cosmetic; '\r' is deserialized
    // faithfully from toml and pem::parse_many is able to parse either style.
    let text = text.replace("\r\n", "\n");
    let certs = pem::parse_many(&text).map_err(Error::new)?;
    if certs.len() == 1 && certs[0].tag == "CERTIFICATE" {
        Ok(text)
    } else {
        Err(Error::msg(format!(
            r#"File "{}" is not a valid certificate PEM"#,
            path
        )))
    }
}

async fn create_acme_key_and_account(
    acme_config: &AcmeConfig,
    domain_name: &str,
) -> Result<(EcPrivateKey, AcmeAccount)> {
    let acme_private_key = {
        let pem = generate_private_key_pem()?;
        EcPrivateKey::from_sec1_pem(&pem)?
    };
    let runtime = sxg_rs::runtime::Runtime {
        acme_signer: Box::new(acme_private_key.create_signer()?),
        fetcher: Box::new(crate::runtime::hyper_fetcher::HyperFetcher::new()),
        ..Default::default()
    };
    let sxg_cert_request_der = sxg_rs::crypto::get_der_from_pem(
        &std::fs::read_to_string(&acme_config.sxg_cert_request_file)?,
        "CERTIFICATE REQUEST",
    )?;
    let eab = if let Some(input_eab) = &acme_config.eab {
        let eab_mac_key =
            base64::decode_config(&input_eab.base64_mac_key, base64::URL_SAFE_NO_PAD)?;
        let eab_signer = OpensslSigner::Hmac(&eab_mac_key);
        let new_account_url =
            AcmeDirectory::from_url(&acme_config.server_url, runtime.fetcher.as_ref())
                .await?
                .0
                .new_account;
        let output_eab = sxg_rs::acme::eab::create_external_account_binding(
            sxg_rs::acme::jws::Algorithm::HS256,
            &input_eab.key_id,
            &new_account_url,
            &acme_private_key.public_key,
            &eab_signer,
        )
        .await?;
        Some(output_eab)
    } else {
        None
    };
    let account = sxg_rs::acme::create_account(
        sxg_rs::acme::AccountSetupParams {
            directory_url: acme_config.server_url.clone(),
            agreed_terms_of_service: &acme_config.agreed_terms_of_service,
            external_account_binding: eab,
            email: &acme_config.contact_email,
            domain: domain_name.to_string(),
            public_key: acme_private_key.public_key.clone(),
            cert_request_der: sxg_cert_request_der,
        },
        runtime.fetcher.as_ref(),
        runtime.acme_signer.as_ref(),
    )
    .await?;
    Ok((acme_private_key, account))
}

fn read_artifact(file_name: &str) -> Result<Artifact> {
    let file_content = std::fs::read_to_string(file_name)?;
    let artifact = serde_yaml::from_str(&file_content)?;
    Ok(artifact)
}

pub fn main(opts: Opts) -> Result<()> {
    if std::env::var("CI").is_ok() && !opts.use_ci_mode {
        println!("The environment variable $CI is set, but --use-ci-mode is not set.");
    }
    goto_repository_root()?;
    let input: Config = serde_yaml::from_str(&std::fs::read_to_string(&opts.input)?)?;
    let mut artifact: Artifact = read_artifact(&opts.artifact).unwrap_or_else(|_| {
        println!("Creating a new artifact");
        Default::default()
    });

    cloudflare::main(
        opts.use_ci_mode,
        &input.sxg_worker,
        &input.certificates,
        &input.cloudflare,
        &mut artifact,
    )?;

    std::fs::write(
        &opts.artifact,
        format!(
            "# This file is generated by command \"cargo run -p tools -- gen-config\".\n\
            # Please do not modify.\n\
            {}",
            serde_yaml::to_string(&artifact)?
        ),
    )?;
    Ok(())
}
