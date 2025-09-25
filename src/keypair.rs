use anyhow::{anyhow, Result};
use log::{info, error};
use solana_sdk::signature::{Keypair, Signature, Signer};
use solana_sdk::pubkey::Pubkey;
use std::fs;
use std::path::Path;

/// Loads a Solana keypair from a JSON file
pub fn load_keypair_from_file<P: AsRef<Path>>(path: P) -> Result<Keypair> {
    let path = path.as_ref();
    info!("Loading keypair from: {}", path.display());
    
    if !path.exists() {
        return Err(anyhow!("Keypair file does not exist: {}", path.display()));
    }

    let content = fs::read_to_string(path)
        .map_err(|e| anyhow!("Failed to read keypair file {}: {}", path.display(), e))?;

    // Try to parse as JSON array (standard Solana format)
    let bytes: Vec<u8> = serde_json::from_str(&content)
        .map_err(|e| anyhow!("Failed to parse keypair file as JSON: {}", e))?;
    
    if bytes.len() != 64 {
        return Err(anyhow!("Invalid keypair file: expected 64 bytes, got {}", bytes.len()));
    }

    let keypair = Keypair::from_bytes(&bytes)
        .map_err(|e| anyhow!("Failed to create keypair from bytes: {}", e))?;

    info!("Successfully loaded keypair with pubkey: {}", keypair.pubkey());
    Ok(keypair)
}

/// Signs data with the provided keypair
pub fn sign_data(keypair: &Keypair, data: &[u8]) -> Signature {
    keypair.sign_message(data)
}

/// Validates that a keypair is working correctly by testing signing/verification
pub fn validate_keypair(keypair: &Keypair) -> Result<()> {
    // Test message for validation
    let test_message = b"keypair_validation_test";
    
    // Try to sign the test message
    let signature = keypair.sign_message(test_message);
    
    // Try to verify the signature
    if signature.verify(keypair.pubkey().as_ref(), test_message) {
        info!("Keypair validation successful for pubkey: {}", keypair.pubkey());
        Ok(())
    } else {
        Err(anyhow!("Keypair validation failed: signature verification failed for pubkey: {}", keypair.pubkey()))
    }
}

/// Verifies a signature against data and public key
pub fn verify_signature(signature: &Signature, data: &[u8], pubkey: &Pubkey) -> bool {
    signature.verify(pubkey.as_ref(), data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;
    use std::io::Write;

    #[test]
    fn test_keypair_creation_and_signing() {
        // Create a test keypair
        let keypair = Keypair::new();
        let test_data = b"test heartbeat data";
        
        // Sign the data
        let signature = sign_data(&keypair, test_data);
        
        // Verify the signature
        assert!(verify_signature(&signature, test_data, &keypair.pubkey()));
    }

    #[test]
    fn test_load_keypair_from_file() {
        // Create a temporary keypair file
        let keypair = Keypair::new();
        let keypair_bytes = keypair.to_bytes();
        let keypair_json = serde_json::to_string(&keypair_bytes.to_vec()).unwrap();
        
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(keypair_json.as_bytes()).unwrap();
        
        // Load the keypair from file
        let loaded_keypair = load_keypair_from_file(temp_file.path()).unwrap();
        
        // Verify they're the same
        assert_eq!(keypair.pubkey(), loaded_keypair.pubkey());
    }
} 