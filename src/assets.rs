use std::{collections::BTreeMap, fmt, sync::Arc};

use serde_json::{Map, Value, json};

use crate::{
    error::{Error, Result},
    retry::random_u64,
};

/// File signatures of the formats the API accepts, and the content type each identifies.
const CONTENT_TYPES: [(&str, &[u8]); 2] = [
    (
        "image/png",
        &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a],
    ),
    ("image/jpeg", &[0xff, 0xd8, 0xff]),
];

const REF_PREFIX: &str = "<image:";
const REF_LEN: usize = REF_PREFIX.len() + 32 + 1;

/// 32 random lowercase hex digits.
pub(crate) fn random_hex() -> String {
    format!("{:016x}{:016x}", random_u64(), random_u64())
}

fn content_type_of(bytes: &[u8]) -> &'static str {
    CONTENT_TYPES
        .iter()
        .find(|(_, sig)| bytes.starts_with(sig))
        .map_or("application/octet-stream", |(ct, _)| ct)
}

/// A PNG or JPEG image asset.
///
/// The API detects the format from the file bytes and rejects other formats with a
/// `BadRequest` error. Its `Display` text is the unique reference it is sent under, so
/// `format!("Compare {a} with {b}")` places images in text, and [`SystemOneRequest::image`]
/// attaches them. (This replaces the TS `withAssets` tagged template.)
///
/// [`SystemOneRequest::image`]: crate::SystemOneRequest::image
#[derive(Clone, Debug)]
pub struct Image {
    data: Arc<[u8]>,
    reference: String,
}

impl Image {
    /// Create an image from file bytes.
    pub fn new(data: impl Into<Vec<u8>>) -> Result<Self> {
        let data: Vec<u8> = data.into();
        if data.is_empty() {
            return Err(Error::sdk("Image data is empty."));
        }
        Ok(Self {
            data: data.into(),
            reference: format!("{REF_PREFIX}{}>", random_hex()),
        })
    }

    /// Create an image from base64 text of the file bytes, without a `data:` URL prefix.
    pub fn from_base64(text: &str) -> Result<Self> {
        Self::new(decode_base64(text)?)
    }

    /// The reference the image is sent under.
    pub fn reference(&self) -> &str {
        &self.reference
    }
}

impl fmt::Display for Image {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.reference)
    }
}

impl From<&Image> for Value {
    fn from(image: &Image) -> Self {
        Value::String(image.reference.clone())
    }
}

// ponytail: tiny std-only decoder instead of a base64 crate; accepts standard alphabet,
// optional padding, and ASCII whitespace.
fn decode_base64(text: &str) -> Result<Vec<u8>> {
    let bad = || Error::sdk("Image data must contain valid base64 text.");
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    let digits = text.trim_end_matches('=');
    for c in digits.bytes().filter(|b| !b.is_ascii_whitespace()) {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return Err(bad()),
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    if bits >= 6 {
        return Err(bad()); // a lone trailing digit carries no whole byte
    }
    if out.is_empty() {
        return Err(Error::sdk("Image data decodes to no bytes."));
    }
    Ok(out)
}

/// An encoded request body.
pub(crate) struct Body {
    pub content_type: String,
    pub bytes: Vec<u8>,
}

/// References (`<image:...>`) found in `json`, in order of appearance.
fn find_references(json: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut rest = json;
    while let Some(i) = rest.find(REF_PREFIX) {
        rest = &rest[i..];
        match rest.get(..REF_LEN) {
            Some(r)
                if r.ends_with('>')
                    && r[REF_PREFIX.len()..REF_LEN - 1]
                        .bytes()
                        .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) =>
            {
                found.push(r);
                rest = &rest[REF_LEN..];
            }
            _ => rest = &rest[REF_PREFIX.len()..],
        }
    }
    found
}

/// Encode `payload` as JSON, or as `multipart/form-data` (RFC 7578) when it sends images.
///
/// The images sent are those in `assets`, then each registered image whose reference appears
/// in the JSON, in order of appearance.
///
/// # Errors
/// The JSON holds the reference of an image not attached to the request, or lacks the name of
/// an asset in `assets`.
pub(crate) fn encode_request(
    mut payload: Map<String, Value>,
    assets: &BTreeMap<String, Image>,
    images: &[Image],
) -> Result<Body> {
    let json = Value::Object(payload.clone()).to_string();

    let unreferenced: Vec<&str> = assets
        .keys()
        .filter(|name| {
            let escaped = Value::String((*name).clone()).to_string();
            !json.contains(&escaped[1..escaped.len() - 1])
        })
        .map(String::as_str)
        .collect();
    if !unreferenced.is_empty() {
        return Err(Error::sdk(format!(
            "Assets not referenced in the request: {}. \
             Write each name in `assets` into `state`, question instructions, or criteria.",
            unreferenced.join(", ")
        )));
    }

    let mut sent: Vec<(String, &Image)> = assets.iter().map(|(n, i)| (n.clone(), i)).collect();
    let mut missing: Vec<&str> = Vec::new();
    for reference in find_references(&json) {
        if sent.iter().any(|(alias, _)| alias == reference) {
            continue;
        }
        match images
            .iter()
            .chain(assets.values())
            .find(|i| i.reference == reference)
        {
            Some(image) => sent.push((reference.to_owned(), image)),
            None => missing.push(reference),
        }
    }
    if !missing.is_empty() {
        return Err(Error::sdk(format!(
            "Image references without an attached image: {}. \
             Attach each image with `SystemOneRequest::image`.",
            missing.join(", ")
        )));
    }

    if sent.is_empty() {
        return Ok(Body {
            content_type: "application/json".into(),
            bytes: json.into_bytes(),
        });
    }

    // The API accepts each part for only one asset, so an image under several references is
    // sent once per reference.
    let mut entries = Map::new();
    for (i, (alias, _)) in sent.iter().enumerate() {
        entries.insert(
            alias.clone(),
            json!({"type": "image", "part": format!("asset-{i}")}),
        );
    }
    payload.insert("assets".into(), Value::Object(entries));

    let boundary = random_hex();
    let mut bytes = Vec::new();
    let mut part = |disposition: String, content_type: &str, content: &[u8]| {
        bytes.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; {disposition}\r\n\
                 Content-Type: {content_type}\r\n\r\n"
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(content);
        bytes.extend_from_slice(b"\r\n");
    };
    part(
        "name=\"request\"".into(),
        "application/json",
        Value::Object(payload).to_string().as_bytes(),
    );
    for (i, (_, image)) in sent.iter().enumerate() {
        let name = format!("asset-{i}");
        part(
            format!("name=\"{name}\"; filename=\"{name}\""),
            content_type_of(&image.data),
            &image.data,
        );
    }
    bytes.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    Ok(Body {
        content_type: format!("multipart/form-data; boundary={boundary}"),
        bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: [u8; 8] = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];

    fn payload(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn base64_roundtrip_and_errors() {
        assert_eq!(decode_base64("aGk=").unwrap(), b"hi");
        assert_eq!(decode_base64("aGk").unwrap(), b"hi");
        assert_eq!(decode_base64("aGVs\nbG8=").unwrap(), b"hello");
        assert!(decode_base64("a$k=").is_err());
        assert!(decode_base64("a").is_err());
        assert!(decode_base64("").is_err());
        assert!(Image::new(Vec::new()).is_err());
    }

    #[test]
    fn plain_json_without_images() {
        let body = encode_request(payload(json!({"state": "hi"})), &BTreeMap::new(), &[]).unwrap();
        assert_eq!(body.content_type, "application/json");
    }

    #[test]
    fn image_reference_is_sent_once_as_multipart() {
        let img = Image::new(PNG).unwrap();
        let p = payload(json!({"state": format!("{img} and {img}")}));
        let body = encode_request(p, &BTreeMap::new(), std::slice::from_ref(&img)).unwrap();
        assert!(
            body.content_type
                .starts_with("multipart/form-data; boundary=")
        );
        let text = String::from_utf8_lossy(&body.bytes);
        assert_eq!(text.matches("Content-Type: image/png").count(), 1);
        assert!(text.contains(&format!(
            "\"{img}\":{{\"part\":\"asset-0\",\"type\":\"image\"}}"
        )));
    }

    #[test]
    fn unattached_reference_is_rejected() {
        let img = Image::new(PNG).unwrap();
        let p = payload(json!({"state": img.to_string()}));
        assert!(encode_request(p, &BTreeMap::new(), &[]).is_err());
    }

    #[test]
    fn named_assets_must_appear() {
        let img = Image::new(PNG).unwrap();
        let assets = BTreeMap::from([("<photo>".to_owned(), img)]);
        assert!(encode_request(payload(json!({"state": "none"})), &assets, &[]).is_err());
        let ok = encode_request(payload(json!({"state": "see <photo>"})), &assets, &[]).unwrap();
        assert!(String::from_utf8_lossy(&ok.bytes).contains("\"<photo>\""));
    }
}
