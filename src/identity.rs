use base64::{Engine as _, engine::general_purpose::STANDARD};
use libp2p::identity::Keypair;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeIdentity {
    pub peer_id: String,
    pub private_key: String,
}

impl NodeIdentity {
    pub fn generate_ed25519() -> Result<Self, IdentityError> {
        let keypair = Keypair::generate_ed25519();
        Self::from_keypair(&keypair)
    }

    pub fn from_private_key(encoded: &str) -> Result<Self, IdentityError> {
        let bytes = STANDARD.decode(encoded)?;
        let keypair = Keypair::from_protobuf_encoding(&bytes)?;
        Self::from_keypair(&keypair)
    }

    fn from_keypair(keypair: &Keypair) -> Result<Self, IdentityError> {
        let peer_id = keypair.public().to_peer_id().to_string();
        let private_key = STANDARD.encode(keypair.to_protobuf_encoding()?);
        Ok(Self {
            peer_id,
            private_key,
        })
    }

    pub fn keypair(&self) -> Result<Keypair, IdentityError> {
        let bytes = STANDARD.decode(&self.private_key)?;
        let keypair = Keypair::from_protobuf_encoding(&bytes)?;
        Ok(keypair)
    }

    pub fn sign(&self, message: &[u8]) -> Result<Vec<u8>, IdentityError> {
        Ok(self.keypair()?.sign(message)?)
    }

    pub fn public_key(&self) -> Result<libp2p::identity::PublicKey, IdentityError> {
        Ok(self.keypair()?.public())
    }

    pub fn public_key_protobuf(&self) -> Result<Vec<u8>, IdentityError> {
        Ok(self.public_key()?.encode_protobuf())
    }
}

#[derive(Debug)]
pub enum IdentityError {
    Base64(base64::DecodeError),
    Libp2p(libp2p::identity::DecodingError),
    Signing(libp2p::identity::SigningError),
}

impl From<base64::DecodeError> for IdentityError {
    fn from(error: base64::DecodeError) -> Self {
        Self::Base64(error)
    }
}

impl From<libp2p::identity::DecodingError> for IdentityError {
    fn from(error: libp2p::identity::DecodingError) -> Self {
        Self::Libp2p(error)
    }
}

impl From<libp2p::identity::SigningError> for IdentityError {
    fn from(error: libp2p::identity::SigningError) -> Self {
        Self::Signing(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_identity_round_trips() {
        let generated = NodeIdentity::generate_ed25519().expect("identity generation");
        let decoded =
            NodeIdentity::from_private_key(&generated.private_key).expect("identity decoding");

        assert_eq!(decoded.peer_id, generated.peer_id);
        assert_eq!(decoded.private_key, generated.private_key);
    }

    #[test]
    fn identity_signatures_verify_with_public_key() {
        let identity = NodeIdentity::generate_ed25519().expect("identity generation");
        let public_key = identity.public_key().expect("public key");
        let signature = identity.sign(b"invite").expect("signature");

        assert!(public_key.verify(b"invite", &signature));
        assert!(!public_key.verify(b"tampered", &signature));
    }
}
