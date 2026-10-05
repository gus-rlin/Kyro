//! Encryption of short-lived authentication and provider credentials. The key
//! belongs to the operator, never to an application record or generated source.
use crate::{AppError, AppResult};
use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit, Payload},
};
use zeroize::Zeroizing;

pub struct CredentialCipher {
    key: Zeroizing<[u8; 32]>,
}
impl CredentialCipher {
    pub fn new(key: [u8; 32]) -> Self {
        Self {
            key: Zeroizing::new(key),
        }
    }
    pub(crate) fn seal(&self, context: &str, plain: &[u8]) -> AppResult<Vec<u8>> {
        if plain.len() > 65536 || context.len() > 512 {
            return Err(AppError::invalid("credential_size_limit"));
        }
        let mut nonce = [0; 12];
        getrandom::fill(&mut nonce).map_err(|_| AppError::Internal)?;
        let cipher =
            Aes256Gcm::new_from_slice(self.key.as_ref()).map_err(|_| AppError::Internal)?;
        let encrypted = cipher
            .encrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: plain,
                    aad: context.as_bytes(),
                },
            )
            .map_err(|_| AppError::Internal)?;
        let mut result = Vec::with_capacity(12 + encrypted.len());
        result.extend_from_slice(&nonce);
        result.extend(encrypted);
        Ok(result)
    }
    pub(crate) fn open(&self, context: &str, sealed: &[u8]) -> AppResult<Zeroizing<Vec<u8>>> {
        if !(28..=65564).contains(&sealed.len()) || context.len() > 512 {
            return Err(AppError::Unauthorized);
        }
        let cipher =
            Aes256Gcm::new_from_slice(self.key.as_ref()).map_err(|_| AppError::Internal)?;
        cipher
            .decrypt(
                &Nonce::from(
                    <[u8; 12]>::try_from(&sealed[..12]).map_err(|_| AppError::Unauthorized)?,
                ),
                Payload {
                    msg: &sealed[12..],
                    aad: context.as_bytes(),
                },
            )
            .map(Zeroizing::new)
            .map_err(|_| AppError::Unauthorized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authenticated_credential_is_bound_to_context() {
        let cipher = CredentialCipher::new([42; 32]);
        let value = cipher
            .seal("tenant/app/principal/mfa", b"synthetic credential")
            .unwrap();
        assert_eq!(
            cipher
                .open("tenant/app/principal/mfa", &value)
                .unwrap()
                .as_slice(),
            b"synthetic credential"
        );
        assert!(cipher.open("other/app/principal/mfa", &value).is_err());
        let mut changed = value;
        changed[13] ^= 1;
        assert!(cipher.open("tenant/app/principal/mfa", &changed).is_err());
    }
}
