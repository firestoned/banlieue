// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `fake_blob.rs`: it refuses what the Blob service refuses.

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::upload::MAX_PAGE_WRITE_BYTES;

    const SIZE: u64 = 8 * 1024 * 1024;

    #[tokio::test]
    async fn refuses_unaligned_oversized_and_out_of_range_writes() {
        let blob = FakePageBlob::new(SIZE);
        assert!(blob.put_page(1, vec![1; 512]).await.is_err());
        assert!(blob.put_page(0, vec![1; 513]).await.is_err());
        assert!(blob.put_page(0, vec![]).await.is_err());
        assert!(
            blob.put_page(0, vec![1; MAX_PAGE_WRITE_BYTES + 512])
                .await
                .is_err()
        );
        assert!(blob.put_page(SIZE, vec![1; 512]).await.is_err());
        assert_eq!(blob.writes(), 0);
        blob.put_page(SIZE - 512, vec![1; 512]).await.unwrap();
        assert_eq!(blob.written_ranges(), vec![(SIZE - 512, 512)]);
    }

    #[tokio::test]
    async fn fails_the_next_writes_with_a_retryable_503() {
        let blob = FakePageBlob::new(SIZE);
        blob.fail_next_writes(1);
        let err = blob.put_page(0, vec![1; 512]).await.unwrap_err();
        assert!(err.is_retryable());
        blob.put_page(0, vec![1; 512]).await.unwrap();
        assert_eq!(blob.writes(), 1);
    }

    #[tokio::test]
    async fn the_check_catches_a_written_zero_range_and_a_missing_one() {
        let chunk = 1024;
        let mut expected = vec![0u8; chunk * 2];
        expected[chunk] = 5;
        let blob = FakePageBlob::new(expected.len() as u64);
        assert!(blob.check_nonzero_written(&expected, chunk).is_err());
        blob.put_page(0, vec![0; chunk]).await.unwrap();
        blob.put_page(chunk as u64, expected[chunk..].to_vec())
            .await
            .unwrap();
        assert!(blob.check_nonzero_written(&expected, chunk).is_err());
        let clean = FakePageBlob::new(expected.len() as u64);
        clean
            .put_page(chunk as u64, expected[chunk..].to_vec())
            .await
            .unwrap();
        clean.check_nonzero_written(&expected, chunk).unwrap();
    }
}
