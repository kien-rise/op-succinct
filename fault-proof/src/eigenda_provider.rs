use alloy_primitives::Bytes;
use alloy_transport_http::reqwest::{Client, Method, Request, Url};
use async_trait::async_trait;
use eigenda_cert::AltDACommitment;
use hex::ToHex;
use hokulea_eigenda::{
    EigenDAPreimageProvider, EncodedPayload, HokuleaErrorKind, HokuleaPreimageError,
};
use hokulea_host_bin::status_code::{
    DerivationError, HostHandlerError, HTTP_RESPONSE_STATUS_CODE_TEAPOT,
};

#[derive(Debug, Clone)]
pub struct OnlineEigenDAPreimageProvider {
    base: Url,
    client: Client,
}

impl OnlineEigenDAPreimageProvider {
    pub fn new_http(base: Url) -> Self {
        let client = Client::new();
        Self { base, client }
    }

    async fn fetch_payload(
        &self,
        altda_commitment: &AltDACommitment,
    ) -> Result<Option<Bytes>, HokuleaErrorKind> {
        let altda_commitment_bytes = Bytes::from(altda_commitment.to_rlp_bytes());
        let commitment_hex = altda_commitment_bytes.encode_hex::<String>();
        tracing::debug!("fetch_payload: {} bytes", altda_commitment_bytes.len());

        let mut url = (self.base.join(&format!("get/{}", commitment_hex))).map_err(|e| {
            tracing::error!("Failed to build URL: {:?}", e);
            HokuleaErrorKind::Critical(format!("build url failed: {:?}", e))
        })?;
        url.query_pairs_mut()
            .append_pair("commitment_mode", "optimism_generic")
            .append_pair("return_encoded_payload", "true");

        let request = Request::new(Method::GET, url.clone());
        let response = self.client.execute(request).await.map_err(|e| {
            tracing::error!("Failed to send request to {}: {:?}", url, e);
            HokuleaErrorKind::Critical(format!("request failed: {:?}", e))
        })?;

        if !response.status().is_success() {
            if response.status().as_u16() != HTTP_RESPONSE_STATUS_CODE_TEAPOT {
                tracing::warn!("EigenDA fetch failed: status {}", response.status());
                return Err(HokuleaErrorKind::Temporary(format!(
                    "fetch failed, status {:?}",
                    response.status()
                )))
            }

            let derivation_error: DerivationError = (response.json().await).map_err(|e| {
                tracing::error!("Failed to deserialize 418 response body: {}", e);
                HokuleaErrorKind::Critical(format!("deserialize 418 failed: {e}"))
            })?;

            match HostHandlerError::from(derivation_error) {
                HostHandlerError::HokuleaPreimageError(HokuleaPreimageError::InvalidCert) => {
                    tracing::debug!("EigenDA cert invalid");
                    Ok(None)
                }
                other => {
                    tracing::error!("Unexpected host handler error: {:?}", other);
                    Err(HokuleaErrorKind::Critical(format!("bad response: {:?}", other)))
                }
            }
        } else {
            let encoded_payload: Bytes = (response.bytes().await)
                .map_err(|e| {
                    tracing::error!("Failed to read response bytes: {}", e);
                    HokuleaErrorKind::Critical(format!("read response bytes failed: {e}"))
                })?
                .into();

            tracing::debug!("Fetched EigenDA payload: {} bytes", encoded_payload.len());

            Ok(Some(encoded_payload))
        }
    }
}

#[async_trait]
impl EigenDAPreimageProvider for OnlineEigenDAPreimageProvider {
    type Error = HokuleaErrorKind;

    /// Query preimage about the validity of a DA cert
    async fn get_validity(
        &mut self,
        altda_commitment: &AltDACommitment,
    ) -> Result<bool, Self::Error> {
        let maybe_payload = self.fetch_payload(altda_commitment).await?;
        Ok(maybe_payload.is_some())
    }

    /// Get encoded payload
    async fn get_encoded_payload(
        &mut self,
        altda_commitment: &AltDACommitment,
    ) -> Result<EncodedPayload, Self::Error> {
        match self.fetch_payload(altda_commitment).await? {
            Some(encoded_payload) => Ok(EncodedPayload { encoded_payload }),
            None => {
                tracing::error!("get_encoded_payload failed: invalid commitment");
                Err(HokuleaErrorKind::Critical("invalid commitment".to_string()))
            }
        }
    }
}
