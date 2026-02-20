/// Minimal DER/X.509 parser — extracts the SPKI (Subject Public Key Info)
/// field from an X.509 certificate encoded in DER.
///
/// X.509 structure (simplified):
/// ```text
/// Certificate ::= SEQUENCE {
///     tbsCertificate  SEQUENCE {
///         version         [0] EXPLICIT INTEGER DEFAULT v1,
///         serialNumber    INTEGER,
///         signature       AlgorithmIdentifier (SEQUENCE),
///         issuer          Name (SEQUENCE),
///         validity        SEQUENCE { notBefore, notAfter },
///         subject         Name (SEQUENCE),
///         subjectPublicKeyInfo  SEQUENCE { ... }  ← target
///         ...
///     },
///     signatureAlgorithm  AlgorithmIdentifier,
///     signatureValue      BIT STRING,
/// }
/// ```
///
/// We navigate the DER TLV (Tag-Length-Value) structure deterministically
/// to reach the 7th field of tbsCertificate. No heap allocation.

/// DER tag constants.
const TAG_SEQUENCE: u8 = 0x30;
const TAG_INTEGER: u8 = 0x02;
const TAG_CONTEXT_0: u8 = 0xA0; // [0] EXPLICIT for version

/// Errors from DER parsing.
#[derive(Debug, Clone, Copy)]
pub enum DerError {
    /// Unexpected end of input.
    Truncated,
    /// Expected a different tag.
    UnexpectedTag,
    /// Length encoding is malformed.
    BadLength,
}

/// A parsed TLV (Tag-Length-Value) element.
struct Tlv<'a> {
    #[allow(dead_code)]
    tag: u8,
    /// The value bytes (does NOT include tag + length header).
    value: &'a [u8],
    /// Total bytes consumed (tag + length header + value).
    total_len: usize,
}

/// Parse a single DER TLV element at the start of `data`.
fn parse_tlv(data: &[u8]) -> Result<Tlv<'_>, DerError> {
    if data.is_empty() {
        return Err(DerError::Truncated);
    }

    let tag = data[0];
    let (value_len, header_len) = parse_length(&data[1..])?;

    let total = header_len + 1 + value_len; // +1 for tag byte
    if total > data.len() {
        return Err(DerError::Truncated);
    }

    Ok(Tlv {
        tag,
        value: &data[header_len + 1..header_len + 1 + value_len],
        total_len: total,
    })
}

/// Parse a DER length field. Returns (value_length, header_bytes_consumed).
fn parse_length(data: &[u8]) -> Result<(usize, usize), DerError> {
    if data.is_empty() {
        return Err(DerError::Truncated);
    }

    let first = data[0];
    if first < 0x80 {
        // Short form: length is the byte itself
        Ok((first as usize, 1))
    } else if first == 0x80 {
        // Indefinite length — not valid in DER
        Err(DerError::BadLength)
    } else {
        // Long form: first byte = 0x80 | num_length_bytes
        let num_bytes = (first & 0x7F) as usize;
        if num_bytes > 4 || num_bytes == 0 {
            return Err(DerError::BadLength);
        }
        if data.len() < 1 + num_bytes {
            return Err(DerError::Truncated);
        }

        let mut len: usize = 0;
        for i in 0..num_bytes {
            len = (len << 8) | (data[1 + i] as usize);
        }

        Ok((len, 1 + num_bytes))
    }
}

/// Skip a TLV element at the start of `data`, returning the remaining bytes.
fn skip_tlv(data: &[u8]) -> Result<&[u8], DerError> {
    let tlv = parse_tlv(data)?;
    Ok(&data[tlv.total_len..])
}

/// Extract the raw SPKI (SubjectPublicKeyInfo) bytes from an X.509 DER
/// certificate. Returns the complete SEQUENCE TLV (tag + length + value),
/// which is what gets hashed for SPKI pinning (RFC 7469).
pub fn extract_spki(cert_der: &[u8]) -> Result<&[u8], DerError> {
    // 1. Parse outer Certificate SEQUENCE
    let outer = parse_tlv(cert_der)?;
    if outer.tag != TAG_SEQUENCE {
        return Err(DerError::UnexpectedTag);
    }
    let inner = outer.value;

    // 2. Parse tbsCertificate SEQUENCE
    let tbs = parse_tlv(inner)?;
    if tbs.tag != TAG_SEQUENCE {
        return Err(DerError::UnexpectedTag);
    }
    let mut pos = tbs.value;

    // 3. Navigate fields inside tbsCertificate:
    //    field 0: version    [0] EXPLICIT (optional, present in v2/v3)
    //    field 1: serialNumber  INTEGER
    //    field 2: signature     SEQUENCE (AlgorithmIdentifier)
    //    field 3: issuer        SEQUENCE (Name)
    //    field 4: validity      SEQUENCE
    //    field 5: subject       SEQUENCE (Name)
    //    field 6: subjectPublicKeyInfo  SEQUENCE  ← we want this

    // Field 0: version — optional, tagged [0]
    if pos.first() == Some(&TAG_CONTEXT_0) {
        pos = skip_tlv(pos)?;
    }

    // Field 1: serialNumber (INTEGER)
    if pos.first() != Some(&TAG_INTEGER) {
        return Err(DerError::UnexpectedTag);
    }
    pos = skip_tlv(pos)?;

    // Field 2: signature (AlgorithmIdentifier = SEQUENCE)
    if pos.first() != Some(&TAG_SEQUENCE) {
        return Err(DerError::UnexpectedTag);
    }
    pos = skip_tlv(pos)?;

    // Field 3: issuer (Name = SEQUENCE)
    if pos.first() != Some(&TAG_SEQUENCE) {
        return Err(DerError::UnexpectedTag);
    }
    pos = skip_tlv(pos)?;

    // Field 4: validity (SEQUENCE)
    if pos.first() != Some(&TAG_SEQUENCE) {
        return Err(DerError::UnexpectedTag);
    }
    pos = skip_tlv(pos)?;

    // Field 5: subject (Name = SEQUENCE)
    if pos.first() != Some(&TAG_SEQUENCE) {
        return Err(DerError::UnexpectedTag);
    }
    pos = skip_tlv(pos)?;

    // Field 6: subjectPublicKeyInfo (SEQUENCE)
    if pos.first() != Some(&TAG_SEQUENCE) {
        return Err(DerError::UnexpectedTag);
    }

    // Return the complete TLV (not just the value) — RFC 7469 hashes
    // the full DER encoding of SubjectPublicKeyInfo.
    let spki = parse_tlv(pos)?;
    Ok(&pos[..spki.total_len])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal self-signed certificate (generated for testing).
    /// This is a real DER-encoded X.509 v3 certificate with ECDSA P-256.
    /// We verify that extract_spki returns the correct SPKI offset.
    #[test]
    fn test_extract_spki_structure() {
        // Build a minimal synthetic DER certificate for structural testing.
        // Certificate = SEQUENCE {
        //   tbsCertificate = SEQUENCE {
        //     version [0] EXPLICIT INTEGER 2,
        //     serialNumber INTEGER 1,
        //     signature SEQUENCE { OID },
        //     issuer SEQUENCE { SET { SEQUENCE { OID, UTF8String } } },
        //     validity SEQUENCE { UTCTime, UTCTime },
        //     subject SEQUENCE { SET { SEQUENCE { OID, UTF8String } } },
        //     subjectPublicKeyInfo SEQUENCE { SEQUENCE { OID }, BIT STRING }
        //   },
        //   signatureAlgorithm SEQUENCE { OID },
        //   signatureValue BIT STRING
        // }

        // Helper: wrap in SEQUENCE tag (0x30)
        fn seq(contents: &[u8]) -> alloc::vec::Vec<u8> {
            let mut out = alloc::vec![0x30];
            encode_length(&mut out, contents.len());
            out.extend_from_slice(contents);
            out
        }

        fn encode_length(out: &mut alloc::vec::Vec<u8>, len: usize) {
            if len < 0x80 {
                out.push(len as u8);
            } else if len < 0x100 {
                out.push(0x81);
                out.push(len as u8);
            } else {
                out.push(0x82);
                out.push((len >> 8) as u8);
                out.push(len as u8);
            }
        }

        fn tagged(tag: u8, contents: &[u8]) -> alloc::vec::Vec<u8> {
            let mut out = alloc::vec![tag];
            encode_length(&mut out, contents.len());
            out.extend_from_slice(contents);
            out
        }

        // version [0] EXPLICIT INTEGER 2
        let version = tagged(0xA0, &tagged(0x02, &[0x02]));
        // serialNumber INTEGER 1
        let serial = tagged(0x02, &[0x01]);
        // signature AlgorithmIdentifier = SEQUENCE { OID }
        let sig_alg = seq(&tagged(0x06, &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x04, 0x03, 0x02]));
        // issuer / subject: SEQUENCE { SET { SEQUENCE { OID, UTF8String "test" } } }
        let name = seq(&tagged(
            0x31,
            &seq(&[
                tagged(0x06, &[0x55, 0x04, 0x03]).as_slice(),
                tagged(0x0C, b"test").as_slice(),
            ]
            .concat()),
        ));
        // validity SEQUENCE { UTCTime, UTCTime }
        let validity = seq(
            &[
                tagged(0x17, b"250101000000Z").as_slice(),
                tagged(0x17, b"350101000000Z").as_slice(),
            ]
            .concat(),
        );
        // subjectPublicKeyInfo — this is what we want to extract
        let spki_content = [
            seq(&tagged(0x06, &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x02, 0x01])).as_slice(),
            tagged(0x03, &[0x00, 0x04, 0xAA, 0xBB, 0xCC, 0xDD]).as_slice(),
        ]
        .concat();
        let spki = seq(&spki_content);

        // tbsCertificate
        let tbs_content = [
            version.as_slice(),
            serial.as_slice(),
            sig_alg.as_slice(),
            name.as_slice(),
            validity.as_slice(),
            name.as_slice(), // subject = issuer for self-signed
            spki.as_slice(),
        ]
        .concat();
        let tbs = seq(&tbs_content);

        // Full certificate
        let cert_content = [
            tbs.as_slice(),
            sig_alg.as_slice(),
            tagged(0x03, &[0x00, 0xDE, 0xAD]).as_slice(),
        ]
        .concat();
        let cert = seq(&cert_content);

        // Extract SPKI
        let extracted = extract_spki(&cert).expect("failed to extract SPKI");
        assert_eq!(extracted, spki.as_slice());
    }

    #[test]
    fn test_parse_length_short() {
        assert_eq!(parse_length(&[0x05]).unwrap(), (5, 1));
        assert_eq!(parse_length(&[0x7F]).unwrap(), (127, 1));
    }

    #[test]
    fn test_parse_length_long() {
        assert_eq!(parse_length(&[0x81, 0x80]).unwrap(), (128, 2));
        assert_eq!(parse_length(&[0x82, 0x01, 0x00]).unwrap(), (256, 3));
    }

    #[test]
    fn test_truncated() {
        assert!(extract_spki(&[]).is_err());
        assert!(extract_spki(&[0x30, 0x03, 0x30, 0x01]).is_err());
    }
}
