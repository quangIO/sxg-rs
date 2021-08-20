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

use std::collections::HashSet;
use serde::{Deserialize, Serialize};

// This struct is source-of-truth of the sxg config. The user need to create
// a file (like `config.yaml`) to provide this config input.
#[derive(Deserialize, Serialize)]
pub struct ConfigInput {
    pub cert_url_basename: String,
    pub forward_request_headers: HashSet<String>,
    pub html_host: String,
    // This field is only needed by Fastly, because Cloudflare uses secret
    // env variables to store private key.
    // TODO: check if Fastly edge dictionary is ok to store private key.
    #[serde(default)]
    pub private_key_base64: String,
    pub strip_request_headers: HashSet<String>,
    pub strip_response_headers: HashSet<String>,
    pub reserved_path: String,
    pub respond_debug_info: bool,
    pub validity_url_basename: String,
    pub worker_host: String,
}

// This contains not only source-of-truth ConfigInput, but also a few more
// attributes which are computed from ConfigInput.
pub struct Config {
    input: ConfigInput,
    pub cert_der: Vec<u8>,
    pub cert_url: String,
    pub issuer_der: Vec<u8>,
    pub validity_url: String,
}

impl std::ops::Deref for Config {
    type Target = ConfigInput;
    #[must_use]
    fn deref(&self) -> &Self::Target {
        &self.input
    }
}

fn lowercase_all(names: HashSet<String>) -> HashSet<String> {
    names.into_iter().map(|h| h.to_ascii_lowercase()).collect()
}

impl Config {
    pub fn new(input_yaml: &str, cert_pem: &str, issuer_pem: &str) -> Self {
        let input: ConfigInput = serde_yaml::from_str(input_yaml).unwrap();
        let cert_der = get_der(cert_pem, "CERTIFICATE");
        let issuer_der = get_der(issuer_pem, "CERTIFICATE");
        let cert_url = create_url(&input.worker_host, &input.reserved_path, &input.cert_url_basename);
        let validity_url = create_url(&input.html_host, &input.reserved_path, &input.validity_url_basename);
        Config {
            cert_der,
            cert_url,
            input: ConfigInput {
                forward_request_headers: lowercase_all(input.forward_request_headers),
                strip_request_headers: lowercase_all(input.strip_request_headers),
                strip_response_headers: lowercase_all(input.strip_response_headers),
                ..input
            },
            issuer_der,
            validity_url,
        }
    }
}

fn get_der(pem_text: &str, expected_tag: &str) -> Vec<u8> {
    for pem in ::pem::parse_many(pem_text) {
        if pem.tag == expected_tag {
            return pem.contents;
        }
    }
    panic!("The PEM file does not contains the expected block");
}

fn create_url(host: &str, reserved_path: &str, basename: &str) -> String {
    let reserved_path = reserved_path.trim_matches('/');
    let basename = basename.trim_start_matches('/');
    format!("https://{}/{}/{}", host, reserved_path, basename)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_create_url() {
        assert_eq!(create_url("foo.com", ".sxg", "cert"), "https://foo.com/.sxg/cert");
        assert_eq!(create_url("foo.com", "/.sxg", "cert"), "https://foo.com/.sxg/cert");
        assert_eq!(create_url("foo.com", ".sxg/", "cert"), "https://foo.com/.sxg/cert");
        assert_eq!(create_url("foo.com", "/.sxg/", "cert"), "https://foo.com/.sxg/cert");
        assert_eq!(create_url("foo.com", "/.sxg/", "/cert"), "https://foo.com/.sxg/cert");
    }
    #[test]
    fn lowercases_header_names() {
        let yaml = r#"
cert_url_basename: "cert"
forward_request_headers:
  - "cf-IPCOUNTRY"
  - "USER-agent"
html_host: my_domain.com
strip_request_headers: ["Forwarded"]
strip_response_headers: ["Set-Cookie", "STRICT-TRANSPORT-SECURITY"]
reserved_path: ".sxg"
respond_debug_info: false
validity_url_basename: "validity"
worker_host: sxg.my_worker_subdomain.workers.dev
        "#;
        // Generated with:
        //   KEY=`mktemp` && CSR=`mktemp` &&
        //   openssl ecparam -out "$KEY" -name prime256v1 -genkey &&
        //   openssl req -new -sha256 -key "$KEY" -out "$CSR" -subj '/CN=example.org/O=Test/C=US' &&
        //   openssl x509 -req -days 90 -in "$CSR" -signkey "$KEY" -out - -extfile <(echo -e "1.3.6.1.4.1.11129.2.1.22 = ASN1:NULL\nsubjectAltName=DNS:example.org") &&
        //   rm "$KEY" "$CSR"
        let cert_pem = "
-----BEGIN CERTIFICATE-----
MIIBkTCCATigAwIBAgIUL/D6t/l3OrSRCI0KlCP7zH1U5/swCgYIKoZIzj0EAwIw
MjEUMBIGA1UEAwwLZXhhbXBsZS5vcmcxDTALBgNVBAoMBFRlc3QxCzAJBgNVBAYT
AlVTMB4XDTIxMDgyMDAwMTc1MFoXDTIxMTExODAwMTc1MFowMjEUMBIGA1UEAwwL
ZXhhbXBsZS5vcmcxDTALBgNVBAoMBFRlc3QxCzAJBgNVBAYTAlVTMFkwEwYHKoZI
zj0CAQYIKoZIzj0DAQcDQgAE3jibTycCk9tifTFg6CyiUirdSlblqLoofEC7B0I4
IO9A52fwDYjZfwGSdu/6ji0MQ1+19Ovr3d9DvXSa7pN1j6MsMCowEAYKKwYBBAHW
eQIBFgQCBQAwFgYDVR0RBA8wDYILZXhhbXBsZS5vcmcwCgYIKoZIzj0EAwIDRwAw
RAIgdTuJ4IXs6LeXQ15TxIsRtfma4F8ypUk0bpBLLbVPbyACIFYul0BjPa2qVd/l
SFfkmh8Fc2QXpbbaK5AQfnQpkDHV
-----END CERTIFICATE-----
        ";
        let config = Config::new(yaml, cert_pem, cert_pem);
        assert_eq!(config.forward_request_headers, ["cf-ipcountry", "user-agent"].iter().map(|s| s.to_string()).collect());
        assert_eq!(config.strip_request_headers, ["forwarded"].iter().map(|s| s.to_string()).collect());
        assert_eq!(config.strip_response_headers, ["set-cookie", "strict-transport-security"].iter().map(|s| s.to_string()).collect());
    }
}