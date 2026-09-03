//! The pictures a message carried, fetched (ADR-0040).
//!
//! Parsing hands over keys; this is where they become bytes, through the
//! message-resource endpoint, and bytes become the one `Image` the journal
//! keeps. What type that is, is read off the bytes: a chat carries what
//! phones and screenshot keys produce, so a BMP goes as PNG and only what no
//! decoder reads is refused (ADR-0041 §2). A picture that cannot be fetched —
//! no `im:resource` scope, a size over the cap, bytes nothing reads — is
//! dropped with a warning, and the words still go: a person who typed a
//! caption is not silenced by the picture beside it.

use bingo_sdk::Image;

use super::api::Api;
use super::event::Picture;

/// The images of `pictures`, in order, minus the ones that did not fetch.
pub async fn fetch(api: &Api, pictures: &[Picture]) -> Vec<Image> {
    let mut images = Vec::with_capacity(pictures.len());
    for picture in pictures {
        match one(api, picture).await {
            Ok(image) => images.push(image),
            Err(why) => tracing::warn!(key = %picture.key, %why, "a picture was dropped"),
        }
    }
    images
}

pub fn resource_path(picture: &Picture) -> String {
    format!(
        "/open-apis/im/v1/messages/{}/resources/{}?type=image",
        picture.message, picture.key
    )
}

async fn one(api: &Api, picture: &Picture) -> Result<Image, String> {
    let bytes = api
        .get_bytes(&resource_path(picture))
        .await
        .map_err(|e| e.to_string())?;
    bingo_pictures::sniffed(&bytes).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bingo_pictures::testing::ImageFormat;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn signed_in(server: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/open-apis/auth/v3/tenant_access_token/internal"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 0, "tenant_access_token": "t-1", "expire": 7200,
            })))
            .mount(server)
            .await;
    }

    fn picture(key: &str) -> Picture {
        Picture {
            message: "om_1".into(),
            key: key.into(),
        }
    }

    async fn served(server: &MockServer, key: &str, response: ResponseTemplate) {
        Mock::given(method("GET"))
            .and(path(format!(
                "/open-apis/im/v1/messages/om_1/resources/{key}"
            )))
            .and(query_param("type", "image"))
            .and(header("authorization", "Bearer t-1"))
            .respond_with(response)
            .mount(server)
            .await;
    }

    /// A picture the endpoint serves, in `format`.
    fn drawn(format: ImageFormat) -> Vec<u8> {
        bingo_pictures::testing::drawn(4, 3, format)
    }

    async fn serving(server: &MockServer, key: &str, bytes: Vec<u8>) {
        served(
            server,
            key,
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/png; charset=binary")
                .set_body_bytes(bytes),
        )
        .await;
    }

    #[tokio::test]
    async fn pictures_are_fetched_in_order_and_a_refused_one_is_dropped() {
        let server = MockServer::start().await;
        signed_in(&server).await;
        let png = drawn(ImageFormat::Png);
        serving(&server, "img_a", png.clone()).await;
        served(&server, "img_gone", ResponseTemplate::new(404)).await;
        serving(&server, "img_b", drawn(ImageFormat::Jpeg)).await;
        let api = Api::new(server.uri(), "cli_1", "secret");
        let images = fetch(
            &api,
            &[picture("img_a"), picture("img_gone"), picture("img_b")],
        )
        .await;
        let types: Vec<&str> = images.iter().map(|i| i.media_type.as_str()).collect();
        assert_eq!(
            types,
            ["image/png", "image/jpeg"],
            "the header said png for both; the bytes did not"
        );
        assert_eq!(
            images[0],
            Image::from_bytes("image/png", &png).expect("within the cap"),
            "a type the table takes is the bytes as they came"
        );
    }

    /// A screenshot off a Windows phone, a scan, a sticker: a chat carries
    /// more than the four types a provider takes, and the journal keeps a
    /// type that replays (ADR-0041 §2).
    #[tokio::test]
    async fn a_type_the_table_refuses_arrives_as_png() {
        let server = MockServer::start().await;
        signed_in(&server).await;
        serving(&server, "img_bmp", drawn(ImageFormat::Bmp)).await;
        serving(&server, "img_tiff", drawn(ImageFormat::Tiff)).await;
        let api = Api::new(server.uri(), "cli_1", "secret");
        let images = fetch(&api, &[picture("img_bmp"), picture("img_tiff")]).await;
        let types: Vec<&str> = images.iter().map(|i| i.media_type.as_str()).collect();
        assert_eq!(types, ["image/png", "image/png"]);
    }

    #[tokio::test]
    async fn bytes_no_decoder_reads_are_dropped_whatever_the_header_says() {
        let server = MockServer::start().await;
        signed_in(&server).await;
        serving(&server, "img_heic", b"not a picture at all".to_vec()).await;
        let api = Api::new(server.uri(), "cli_1", "secret");
        assert!(fetch(&api, &[picture("img_heic")]).await.is_empty());
    }
}
