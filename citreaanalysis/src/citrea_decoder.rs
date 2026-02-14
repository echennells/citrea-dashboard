use serde::{Deserialize, Serialize};

/// Citrea sequencer commitment decoded from a Bitcoin transaction witness
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CitreaCommitment {
    /// Bitcoin transaction ID
    pub txid: String,
    /// Block height where the transaction was confirmed
    pub block_height: Option<u64>,
    /// Block timestamp
    pub block_time: Option<u64>,
    /// The sequencer's x-only public key (32 bytes, hex encoded)
    pub sequencer_pubkey: String,
    /// Header/kind value from the inscription
    pub header: u16,
    /// ECDSA signature over the body (64 bytes, hex encoded)
    pub signature: String,
    /// The body data containing the commitment
    pub body: CommitmentBody,
    /// Nonce value (prevents txid collisions)
    pub nonce: u64,
    /// Total length of the body in the witness script.
    /// Commitments are 45 bytes; proofs are typically 26,000-39,000 bytes.
    pub body_length: usize,
}

/// The decoded body of a Citrea commitment
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitmentBody {
    /// Batch proof type/kind
    pub kind: u8,
    /// 32-byte state commitment hash (hex encoded)
    pub state_commitment: String,
    /// Batch number
    pub batch_number: u32,
    /// L2 block height
    pub l2_block_height: u32,
    /// Reserved/padding field
    pub reserved: u32,
}

/// Errors that can occur when decoding Citrea transactions
#[derive(Debug)]
pub enum DecodeError {
    InvalidWitnessStructure(String),
    InvalidScriptStructure(String),
    InvalidBodyLength(usize),
    HexDecodeError(String),
    MissingWitnessData,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::InvalidWitnessStructure(msg) => {
                write!(f, "Invalid witness structure: {}", msg)
            }
            DecodeError::InvalidScriptStructure(msg) => {
                write!(f, "Invalid script structure: {}", msg)
            }
            DecodeError::InvalidBodyLength(len) => {
                write!(f, "Invalid body length: {} (expected >= 45)", len)
            }
            DecodeError::HexDecodeError(msg) => write!(f, "Hex decode error: {}", msg),
            DecodeError::MissingWitnessData => write!(f, "Missing witness data"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Decode a Citrea commitment from witness hex strings (from API response)
pub fn decode_from_witness(txid: &str, witness: &[String]) -> Result<CitreaCommitment, DecodeError> {
    let witness_bytes: Result<Vec<Vec<u8>>, _> = witness
        .iter()
        .map(|w| hex::decode(w))
        .collect();

    let witness_bytes = witness_bytes.map_err(|e| DecodeError::HexDecodeError(e.to_string()))?;
    decode_witness_elements(txid, &witness_bytes)
}

/// Decode a Citrea commitment from raw transaction hex
pub fn decode_from_tx_hex(txid: &str, tx_hex: &str) -> Result<CitreaCommitment, DecodeError> {
    let tx_bytes = hex::decode(tx_hex).map_err(|e| DecodeError::HexDecodeError(e.to_string()))?;

    // Parse the transaction to extract witness data
    // Transaction format: version (4) + marker (1) + flag (1) + inputs + outputs + witness + locktime (4)

    if tx_bytes.len() < 10 {
        return Err(DecodeError::InvalidWitnessStructure(
            "Transaction too short".to_string(),
        ));
    }

    // Check for segwit marker and flag (0x00, 0x01)
    if tx_bytes[4] != 0x00 || tx_bytes[5] != 0x01 {
        return Err(DecodeError::InvalidWitnessStructure(
            "Not a segwit transaction".to_string(),
        ));
    }

    // Find the witness section by parsing through inputs and outputs
    let mut pos = 6; // After version + marker + flag

    // Parse input count (varint)
    let (input_count, varint_len) = read_varint(&tx_bytes[pos..])?;
    pos += varint_len;

    // Skip inputs (each input: prevout(36) + script_sig_len(varint) + script_sig + sequence(4))
    for _ in 0..input_count {
        pos += 36; // prevout (txid + vout)
        let (script_len, varint_len) = read_varint(&tx_bytes[pos..])?;
        pos += varint_len + script_len as usize + 4; // script + sequence
    }

    // Parse output count
    let (output_count, varint_len) = read_varint(&tx_bytes[pos..])?;
    pos += varint_len;

    // Skip outputs
    for _ in 0..output_count {
        pos += 8; // value
        let (script_len, varint_len) = read_varint(&tx_bytes[pos..])?;
        pos += varint_len + script_len as usize;
    }

    // Now we're at the witness section
    // For each input, read witness stack
    for input_idx in 0..input_count {
        let (stack_items, varint_len) = read_varint(&tx_bytes[pos..])?;
        pos += varint_len;

        if input_idx == 0 && stack_items >= 3 {
            // This is the input we care about - should have signature, script, control block
            let mut witness_elements: Vec<Vec<u8>> = Vec::new();

            for _ in 0..stack_items {
                let (item_len, varint_len) = read_varint(&tx_bytes[pos..])?;
                pos += varint_len;
                witness_elements.push(tx_bytes[pos..pos + item_len as usize].to_vec());
                pos += item_len as usize;
            }

            // Parse the witness elements
            return decode_witness_elements(txid, &witness_elements);
        } else {
            // Skip this input's witness
            for _ in 0..stack_items {
                let (item_len, varint_len) = read_varint(&tx_bytes[pos..])?;
                pos += varint_len + item_len as usize;
            }
        }
    }

    Err(DecodeError::MissingWitnessData)
}

/// Decode Citrea commitment from witness elements
fn decode_witness_elements(
    txid: &str,
    elements: &[Vec<u8>],
) -> Result<CitreaCommitment, DecodeError> {
    if elements.len() < 3 {
        return Err(DecodeError::InvalidWitnessStructure(format!(
            "Expected 3+ witness elements, got {}",
            elements.len()
        )));
    }

    let schnorr_sig = &elements[0];
    let script = &elements[1];
    let _control_block = &elements[2];

    if schnorr_sig.len() != 64 {
        return Err(DecodeError::InvalidWitnessStructure(format!(
            "Expected 64-byte Schnorr signature, got {}",
            schnorr_sig.len()
        )));
    }

    // Parse the script
    // Expected structure:
    // OP_PUSHBYTES_32 <pubkey> OP_CHECKSIGVERIFY OP_PUSHBYTES_2 <header> OP_0 OP_IF
    // OP_PUSHBYTES_64 <signature> OP_PUSHBYTES_33 <pubkey> OP_PUSHBYTES_45 <body>
    // OP_ENDIF OP_PUSHBYTES_8 <nonce> OP_NIP

    let mut pos = 0;

    // OP_PUSHBYTES_32 (0x20)
    if script.get(pos) != Some(&0x20) {
        return Err(DecodeError::InvalidScriptStructure(
            "Expected OP_PUSHBYTES_32".to_string(),
        ));
    }
    pos += 1;

    // X-only public key (32 bytes)
    let sequencer_pubkey = hex::encode(&script[pos..pos + 32]);
    pos += 32;

    // OP_CHECKSIGVERIFY (0xad)
    if script.get(pos) != Some(&0xad) {
        return Err(DecodeError::InvalidScriptStructure(
            "Expected OP_CHECKSIGVERIFY".to_string(),
        ));
    }
    pos += 1;

    // OP_PUSHBYTES_2 (0x02)
    if script.get(pos) != Some(&0x02) {
        return Err(DecodeError::InvalidScriptStructure(
            "Expected OP_PUSHBYTES_2 for header".to_string(),
        ));
    }
    pos += 1;

    // Header (2 bytes, little-endian)
    let header = u16::from_le_bytes([script[pos], script[pos + 1]]);
    pos += 2;

    // OP_0 (0x00)
    if script.get(pos) != Some(&0x00) {
        return Err(DecodeError::InvalidScriptStructure(
            "Expected OP_0".to_string(),
        ));
    }
    pos += 1;

    // OP_IF (0x63)
    if script.get(pos) != Some(&0x63) {
        return Err(DecodeError::InvalidScriptStructure(
            "Expected OP_IF".to_string(),
        ));
    }
    pos += 1;

    // OP_PUSHBYTES_64 (0x40)
    if script.get(pos) != Some(&0x40) {
        return Err(DecodeError::InvalidScriptStructure(
            "Expected OP_PUSHBYTES_64 for signature".to_string(),
        ));
    }
    pos += 1;

    // ECDSA signature (64 bytes)
    let signature = hex::encode(&script[pos..pos + 64]);
    pos += 64;

    // OP_PUSHBYTES_33 (0x21)
    if script.get(pos) != Some(&0x21) {
        return Err(DecodeError::InvalidScriptStructure(
            "Expected OP_PUSHBYTES_33 for pubkey".to_string(),
        ));
    }
    pos += 1;

    // Compressed public key (33 bytes) - skip it
    pos += 33;

    // Read body push opcode: OP_PUSHBYTES_N (0x01..0x4b), OP_PUSHDATA1 (0x4c),
    // or OP_PUSHDATA2 (0x4d). Commitments are 45 bytes; proofs are much larger.
    let body_opcode = *script.get(pos).ok_or_else(|| {
        DecodeError::InvalidScriptStructure("Unexpected end of script at body push opcode".to_string())
    })?;
    pos += 1;

    let body_length: usize = if body_opcode >= 0x01 && body_opcode <= 0x4b {
        // OP_PUSHBYTES_N: the opcode byte IS the length
        body_opcode as usize
    } else if body_opcode == 0x4c {
        // OP_PUSHDATA1: next 1 byte is the length
        let len = *script.get(pos).ok_or_else(|| {
            DecodeError::InvalidScriptStructure("Truncated OP_PUSHDATA1 length".to_string())
        })? as usize;
        pos += 1;
        len
    } else if body_opcode == 0x4d {
        // OP_PUSHDATA2: next 2 bytes are the length (little-endian)
        if pos + 2 > script.len() {
            return Err(DecodeError::InvalidScriptStructure(
                "Truncated OP_PUSHDATA2 length".to_string(),
            ));
        }
        let len = u16::from_le_bytes([script[pos], script[pos + 1]]) as usize;
        pos += 2;
        len
    } else {
        return Err(DecodeError::InvalidScriptStructure(format!(
            "Unexpected body push opcode: 0x{:02x}",
            body_opcode
        )));
    };

    if pos + body_length > script.len() {
        return Err(DecodeError::InvalidScriptStructure(format!(
            "Body length {} exceeds remaining script ({} bytes left)",
            body_length,
            script.len() - pos
        )));
    }

    // Parse the first 45 bytes of the body (shared header for both commitments and proofs)
    if body_length < 45 {
        return Err(DecodeError::InvalidBodyLength(body_length));
    }
    let body_bytes = &script[pos..pos + 45];
    let body = decode_body(body_bytes)?;
    pos += body_length;

    // OP_ENDIF (0x68)
    if script.get(pos) != Some(&0x68) {
        return Err(DecodeError::InvalidScriptStructure(
            "Expected OP_ENDIF".to_string(),
        ));
    }
    pos += 1;

    // OP_PUSHBYTES_8 (0x08)
    if script.get(pos) != Some(&0x08) {
        return Err(DecodeError::InvalidScriptStructure(
            "Expected OP_PUSHBYTES_8 for nonce".to_string(),
        ));
    }
    pos += 1;

    // Nonce (8 bytes, little-endian)
    let nonce = u64::from_le_bytes(script[pos..pos + 8].try_into().unwrap());

    Ok(CitreaCommitment {
        txid: txid.to_string(),
        block_height: None,
        block_time: None,
        sequencer_pubkey,
        header,
        signature,
        body,
        nonce,
        body_length,
    })
}

/// Decode the 45-byte body into structured data
fn decode_body(body: &[u8]) -> Result<CommitmentBody, DecodeError> {
    if body.len() != 45 {
        return Err(DecodeError::InvalidBodyLength(body.len()));
    }

    let kind = body[0];
    let state_commitment = hex::encode(&body[1..33]);
    let batch_number = u32::from_le_bytes(body[33..37].try_into().unwrap());
    let l2_block_height = u32::from_le_bytes(body[37..41].try_into().unwrap());
    let reserved = u32::from_le_bytes(body[41..45].try_into().unwrap());

    Ok(CommitmentBody {
        kind,
        state_commitment,
        batch_number,
        l2_block_height,
        reserved,
    })
}

/// Read a Bitcoin varint from a byte slice
fn read_varint(bytes: &[u8]) -> Result<(u64, usize), DecodeError> {
    if bytes.is_empty() {
        return Err(DecodeError::InvalidWitnessStructure(
            "Empty varint".to_string(),
        ));
    }

    match bytes[0] {
        0..=0xfc => Ok((bytes[0] as u64, 1)),
        0xfd => {
            if bytes.len() < 3 {
                return Err(DecodeError::InvalidWitnessStructure(
                    "Truncated varint".to_string(),
                ));
            }
            Ok((u16::from_le_bytes([bytes[1], bytes[2]]) as u64, 3))
        }
        0xfe => {
            if bytes.len() < 5 {
                return Err(DecodeError::InvalidWitnessStructure(
                    "Truncated varint".to_string(),
                ));
            }
            Ok((
                u32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]) as u64,
                5,
            ))
        }
        0xff => {
            if bytes.len() < 9 {
                return Err(DecodeError::InvalidWitnessStructure(
                    "Truncated varint".to_string(),
                ));
            }
            Ok((
                u64::from_le_bytes([
                    bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7], bytes[8],
                ]),
                9,
            ))
        }
    }
}

impl CitreaCommitment {
    /// Get the state commitment as a hex string
    pub fn state_commitment_hex(&self) -> &str {
        &self.body.state_commitment
    }

    /// Get the sequencer pubkey as a hex string
    pub fn sequencer_pubkey_hex(&self) -> &str {
        &self.sequencer_pubkey
    }
}

impl std::fmt::Display for CitreaCommitment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Citrea Commitment")?;
        writeln!(f, "  TXID: {}", self.txid)?;
        if let Some(height) = self.block_height {
            writeln!(f, "  Block Height: {}", height)?;
        }
        writeln!(f, "  Batch Number: {}", self.body.batch_number)?;
        writeln!(f, "  L2 Block: {}", self.body.l2_block_height)?;
        writeln!(f, "  State Commitment: {}", self.state_commitment_hex())?;
        writeln!(f, "  Header: 0x{:04x}", self.header)?;
        writeln!(f, "  Nonce: {}", self.nonce)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_known_transaction() {
        // The transaction from the user's example
        let tx_hex = "020000000001010f77bd4ec3c88004f3d6345377425892c1fec7df61362f87a0a7ad492b2276520000000000fdffffff0189080000000000002251200024315b1e282c8242af227953c0e47d5b5171aad626ba9c0d29f6477cbff3c503408497b550da6be57122a56cb7e187486585e745e2f7f63162581bbe75bf6c601969d9e864f473bb3d4086bbbcc24797544b897c331b676874f45a4e799664467cc3202a31a1fa359abd2e6fc1136b4dea711e5f18618504e021084cc61099f72bb2bdad020400006340850f4bf1eb24a0a53d88828e9fc1316c40806c8590cfd16cd3116daa7adcad657dafe0782d9da63fe793b31237ab648caee7f620b0f3b5847af27714b193731321032a31a1fa359abd2e6fc1136b4dea711e5f18618504e021084cc61099f72bb2bd2d046d74b933a94ee0c7114a8b75b30bfb2cf821775c2bb7fe02b4383c6b949986a0d90a0000a85f2a000000000068081a000000000000007721c12a31a1fa359abd2e6fc1136b4dea711e5f18618504e021084cc61099f72bb2bd00000000";
        let txid = "d830520603928be72ebd93c1d49a218570bfadbb2d2786e19378d1f149c2951b";

        let commitment = decode_from_tx_hex(txid, tx_hex).expect("Failed to decode");

        assert_eq!(commitment.body.batch_number, 2777);
        assert_eq!(commitment.body.l2_block_height, 2777000);
        assert_eq!(commitment.body.kind, 4);
        assert_eq!(commitment.header, 4);
        assert_eq!(commitment.nonce, 26);
        assert_eq!(
            commitment.body.state_commitment,
            "6d74b933a94ee0c7114a8b75b30bfb2cf821775c2bb7fe02b4383c6b949986a0"
        );
        assert_eq!(
            commitment.sequencer_pubkey,
            "2a31a1fa359abd2e6fc1136b4dea711e5f18618504e021084cc61099f72bb2bd"
        );
    }
}
