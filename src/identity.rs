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
}

#[derive(Debug)]
pub enum IdentityError {
    Base64(base64::DecodeError),
    Libp2p(libp2p::identity::DecodingError),
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
}
