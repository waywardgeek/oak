//
// Copyright 2023 The Project Oak Authors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//

//! Implementation of the Bidirectional Hybrid Public Key Encryption (HPKE) scheme from RFC9180.
//! <https://www.rfc-editor.org/rfc/rfc9180.html>
//! <https://www.rfc-editor.org/rfc/rfc9180.html#name-bidirectional-encryption>

#![no_std]

extern crate alloc;

pub mod schema {
    #![allow(dead_code)]
    include!(concat!(env!("OUT_DIR"), "/oak.crypto.rs"));
}

pub mod aead;
pub mod hpke;
#[cfg(test)]
mod tests;
pub mod util;

use crate::hpke::{
    setup_base_recipient, setup_base_sender, KeyPair, RecipientContext, RecipientResponseContext,
    SenderContext, SenderResponseContext,
};
use alloc::vec::Vec;
use anyhow::Context;

/// Info string used by Hybrid Public Key Encryption;
const OAK_HPKE_INFO: &[u8] = b"Oak Hybrid Public Key Encryption v1";

/// Implementation of the HPKE sender.
/// Expects a serialized HPKE recipient public key and uses it to create encryptors for secure
/// bidirectional sessions with the HPKE recipient.
///
/// Each new call to the [`SenderCryptoProvider::create_encryptor`] creates a new ephemeral key pair
/// which will not be reused for other sessions. So each [`SenderCryptoProvider::create_encryptor`]
/// call represents a new HPKE session.
///
/// To prevent from reusing same encryptors and decryptors for multiple sessions, each call to the
/// [`SenderRequestEncryptor::encrypt`] consumes the corresponding encryptor and produces a
/// decryptor for the response message. And each call to [`SenderResponseDecryptor::decrypt`]
/// consumes the corresponding decryptor and produces a new encryptor for encrypting a new resuest
/// within the same session.
pub struct SenderCryptoProvider {
    serialized_recipient_public_key: Vec<u8>,
}

impl SenderCryptoProvider {
    /// Creates a new sender crypto provider.
    /// The `serialized_recipient_public_key` must be a NIST P-256 SEC1 encoded point public key.
    /// <https://secg.org/sec1-v2.pdf>
    pub fn new(serialized_recipient_public_key: &[u8]) -> Self {
        Self {
            serialized_recipient_public_key: serialized_recipient_public_key.to_vec(),
        }
    }

    /// Creates an HPKE encryptor by generating an new ephemeral key pair.
    /// Returns a serialized encapsulated ephemeral public key and a [`SenderRequestEncryptor`].
    /// The ephemeral public key is a NIST P-256 SEC1 encoded point public key.
    /// <https://secg.org/sec1-v2.pdf>
    pub fn create_encryptor(&self) -> anyhow::Result<(Vec<u8>, SenderRequestEncryptor)> {
        let (serialized_encapsulated_public_key, sender_context, sender_response_context) =
            setup_base_sender(&self.serialized_recipient_public_key, OAK_HPKE_INFO)
                .context("couldn't create sender request encryptor")?;
        Ok((
            serialized_encapsulated_public_key.to_vec(),
            SenderRequestEncryptor {
                sender_context,
                sender_response_context,
            },
        ))
    }
}

/// Encryptor for sender requests that will be sent to the recipient.
pub struct SenderRequestEncryptor {
    sender_context: SenderContext,
    sender_response_context: SenderResponseContext,
}

impl SenderRequestEncryptor {
    /// Encrypts `plaintext` and authenticates `associated_data` using AEAD.
    /// Returns a request message ciphertext and a corresponding response decryptor.
    /// <https://datatracker.ietf.org/doc/html/rfc5116>
    pub fn encrypt(
        mut self,
        plaintext: &[u8],
        associated_data: &[u8],
    ) -> anyhow::Result<(Vec<u8>, SenderResponseDecryptor)> {
        let request = self
            .sender_context
            .seal(plaintext, associated_data)
            .context("couldn't encrypt request")?;
        let decryptor = SenderResponseDecryptor {
            sender_context: self.sender_context,
            sender_response_context: self.sender_response_context,
        };
        Ok((request, decryptor))
    }
}

/// Decryptor for recipient responses that are received by the sender.
pub struct SenderResponseDecryptor {
    sender_context: SenderContext,
    sender_response_context: SenderResponseContext,
}

impl SenderResponseDecryptor {
    /// Decrypts `ciphertext` and authenticates `associated_data` using AEAD.
    /// Returns a response message plaintext and a request encryptor for encrypting a new request
    /// within the same session.
    /// <https://datatracker.ietf.org/doc/html/rfc5116>
    pub fn decrypt(
        mut self,
        ciphertext: &[u8],
        associated_data: &[u8],
    ) -> anyhow::Result<(Vec<u8>, SenderRequestEncryptor)> {
        let response = self
            .sender_response_context
            .open(ciphertext, associated_data)
            .context("couldn't decrypt response")?;
        let encryptor = SenderRequestEncryptor {
            sender_context: self.sender_context,
            sender_response_context: self.sender_response_context,
        };
        Ok((response, encryptor))
    }
}

/// Implementation of the HPKE recipient.
/// Generates a key pair and creates HPKE decryptors for secure bidirectional sessions with HPKE
/// senders.
///
/// Each new call to the [`RecipientCryptoProvider::create_decryptor`] creates a new decryptor using
/// a serialized ephemeral sender public key and represents a new HPKE session.
///
/// To prevent from reusing same encryptors and decryptors for multiple sessions, each call to the
/// [`RecipientRequestDecryptor::decrypt`] consumes the corresponding decryptor and produces an
/// encryptor for the response message. And each call to [`RecipientResponseEncryptor::encrypt`]
/// consumes the corresponding encryptor and produces a new decryptor for decrypting a new request
/// within the same session.
///
/// Public key that corresponds to the generated key pair can be used by HPKE senders to derive
/// encryption keys for each secure bidirectional session.
pub struct RecipientCryptoProvider {
    key_pair: KeyPair,
}

impl Default for RecipientCryptoProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl RecipientCryptoProvider {
    /// Creates a recipient crypto provider with a newly generated key pair.
    pub fn new() -> Self {
        Self {
            key_pair: KeyPair::generate(),
        }
    }

    /// Returns a NIST P-256 SEC1 encoded point public key.
    /// <https://secg.org/sec1-v2.pdf>
    pub fn get_serialized_public_key(&self) -> Vec<u8> {
        self.key_pair.get_serialized_public_key()
    }

    /// Creates an HPKE decryptor using a serialized ephemeral sender public key.
    /// The `serialized_encapsulated_public_key` must be a NIST P-256 SEC1 encoded point public key.
    /// <https://secg.org/sec1-v2.pdf>
    pub fn create_decryptor(
        &self,
        serialized_encapsulated_public_key: &[u8],
    ) -> anyhow::Result<RecipientRequestDecryptor> {
        let (recipient_context, recipient_response_context) = setup_base_recipient(
            serialized_encapsulated_public_key,
            &self.key_pair,
            OAK_HPKE_INFO,
        )
        .context("couldn't create recipient request decryptor")?;
        Ok(RecipientRequestDecryptor {
            recipient_context,
            recipient_response_context,
        })
    }
}

/// Decryptor for sender requests that are received by the recipient.
pub struct RecipientRequestDecryptor {
    recipient_context: RecipientContext,
    recipient_response_context: RecipientResponseContext,
}

impl RecipientRequestDecryptor {
    /// Decrypts `ciphertext` and authenticates `associated_data` using AEAD.
    /// Returns a request message plaintext and a corresponding response encryptor.
    /// <https://datatracker.ietf.org/doc/html/rfc5116>
    pub fn decrypt(
        mut self,
        ciphertext: &[u8],
        associated_data: &[u8],
    ) -> anyhow::Result<(Vec<u8>, RecipientResponseEncryptor)> {
        let plaintext = self
            .recipient_context
            .open(ciphertext, associated_data)
            .context("couldn't decrypt request")?;
        let encryptor = RecipientResponseEncryptor {
            recipient_context: self.recipient_context,
            recipient_response_context: self.recipient_response_context,
        };
        Ok((plaintext, encryptor))
    }
}

/// Encryptor for recipient responses that will be sent to the sender.
pub struct RecipientResponseEncryptor {
    recipient_context: RecipientContext,
    recipient_response_context: RecipientResponseContext,
}

impl RecipientResponseEncryptor {
    /// Encrypts `plaintext` and authenticates `associated_data` using AEAD.
    /// Returns a response message ciphertext and a request decryptor for decrypting a new request
    /// within the same session.
    /// <https://datatracker.ietf.org/doc/html/rfc5116>
    pub fn encrypt(
        mut self,
        plaintext: &[u8],
        associated_data: &[u8],
    ) -> anyhow::Result<(Vec<u8>, RecipientRequestDecryptor)> {
        let response = self
            .recipient_response_context
            .seal(plaintext, associated_data)
            .context("couldn't encrypt response")?;
        let encryptor = RecipientRequestDecryptor {
            recipient_context: self.recipient_context,
            recipient_response_context: self.recipient_response_context,
        };
        Ok((response, encryptor))
    }
}
