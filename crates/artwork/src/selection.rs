//! Decodes stored artwork bindings into images in selection order.

pub(crate) use library::ArtworkCandidate as Candidate;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ArtworkBinding {
    pub(crate) candidates: Vec<Candidate>,
    asset_identity: String,
    stable_identity: String,
}

impl ArtworkBinding {
    pub fn opaque(binding: &[u8]) -> Self {
        let mut candidates: Vec<Candidate> = serde_json::from_slice(binding).unwrap_or_default();
        for candidate in &mut candidates {
            if candidate.is_automatic_playlist() {
                *candidate = Candidate::new(candidate.image.clone(), candidate.blur_hash.clone());
            }
        }
        let asset_identity = candidates
            .first()
            .map(Candidate::asset_identity)
            .unwrap_or_default();
        let stable_identity = candidates
            .iter()
            .map(Candidate::stable_identity)
            .collect::<Vec<_>>()
            .join("\0");
        Self {
            candidates,
            asset_identity,
            stable_identity,
        }
    }

    /// Whether any candidate is a server image.
    pub(crate) fn is_native(&self) -> bool {
        self.candidates.iter().any(Candidate::is_native)
    }

    pub(crate) fn asset_identity(&self) -> &str {
        &self.asset_identity
    }

    pub fn stable_identity(&self) -> &str {
        &self.stable_identity
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_binding_decodes_exactly_one_selected_source_request() {
        let encoded = sources::native_artwork_binding(&sources::NativeImageRef::new(
            "album",
            Some("tag".to_string()),
        ))
        .expect("encode binding");
        let binding = ArtworkBinding::opaque(&encoded);
        assert!(!binding.candidates.is_empty());
        assert!(binding.stable_identity().contains("album"));
        let mut candidates = binding.candidates.clone();
        for candidate in &mut candidates {
            candidate.blur_hash = Some("LEHV6nWB2yk8pyo0adR*.7kCMdnj".into());
        }
        let after = ArtworkBinding::opaque(&serde_json::to_vec(&candidates).unwrap());
        assert_eq!(after.stable_identity(), binding.stable_identity());
        assert_eq!(after.asset_identity(), binding.asset_identity());
        assert!(
            after
                .candidates
                .iter()
                .all(|candidate| candidate.blur_hash().is_some())
        );
    }
}
