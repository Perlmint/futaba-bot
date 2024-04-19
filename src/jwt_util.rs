use jwt::VerifyingAlgorithm;
use rsa::{pkcs8::AssociatedOid, Pkcs1v15Sign, RsaPublicKey};
use sha2::Digest;

pub enum RsAlgorithm {
    Rs256,
    Rs384,
    Rs512,
}
pub struct RsaVerifying(pub RsaPublicKey, pub RsAlgorithm);

impl RsaVerifying {
    fn verify_with_hash<H: Digest + AssociatedOid>(
        &self,
        header: &str,
        claims: &str,
        signature: &[u8],
    ) -> Result<bool, jwt::Error> {
        match self.0.verify(
            Pkcs1v15Sign::new::<H>(),
            {
                let mut hasher = H::new();
                hasher.update(header);
                hasher.update(".");
                hasher.update(claims);
                &hasher.finalize()
            },
            signature,
        ) {
            Ok(()) => Ok(true),
            Err(e) if e == rsa::Error::Verification => Ok(false),
            Err(_) => Err(jwt::Error::InvalidSignature),
        }
    }
}

impl VerifyingAlgorithm for RsaVerifying {
    fn algorithm_type(&self) -> jwt::AlgorithmType {
        match self.1 {
            RsAlgorithm::Rs256 => jwt::AlgorithmType::Rs256,
            RsAlgorithm::Rs384 => jwt::AlgorithmType::Rs384,
            RsAlgorithm::Rs512 => jwt::AlgorithmType::Rs512,
        }
    }

    fn verify_bytes(
        &self,
        header: &str,
        claims: &str,
        signature: &[u8],
    ) -> Result<bool, jwt::Error> {
        match self.1 {
            RsAlgorithm::Rs256 => self.verify_with_hash::<sha2::Sha256>(header, claims, signature),
            RsAlgorithm::Rs384 => self.verify_with_hash::<sha2::Sha384>(header, claims, signature),
            RsAlgorithm::Rs512 => self.verify_with_hash::<sha2::Sha512>(header, claims, signature),
        }
    }
}
