use super::*;

#[derive(Clone)]
/// A keystore for password-based keypairs.
pub struct PasswordKeypairStorage {
	/// The keystore container.
	pub inner: KeystoreContainer,
	/// The secret for encryption/decryption.
	pub secret: Option<SecretString>,
}

impl PasswordKeypairStorage {
	pub fn new(path: String, network: Network, secret: Option<SecretString>) -> Arc<RwLock<Self>> {
		Arc::new(RwLock::new(Self { inner: KeystoreContainer::new(path, network), secret }))
	}
}

#[async_trait::async_trait]
impl KeypairStorageT for PasswordKeypairStorage {
	async fn load(&mut self, round: u32) {
		self.inner.load(round);
	}

	async fn load_v1(&mut self, round: u32, secret: Option<SecretString>) {
		self.inner.load_v1(round, secret);
	}

	async fn db(&self) -> Arc<LocalKeystore> {
		self.inner.db()
	}

	async fn sign_psbt_inner(
		&self,
		psbt: &mut Psbt,
	) -> Result<SigningKeys, (SigningKeys, SigningErrors)> {
		psbt.sign(self, &Secp256k1::signing_only())
	}

	async fn keys(&self) -> Vec<Vec<u8>> {
		self.inner.db().keys(ECDSA).expect("Failed to load keys")
	}

	async fn raw_keystore_value(&self, key: &[u8]) -> Option<String> {
		self.inner
			.db()
			.raw_keystore_value::<AppPair>(
				&AppPublic::from_slice(key).expect(KEYSTORE_INTERNAL_ERROR),
			)
			.expect(KEYSTORE_INTERNAL_ERROR)
	}

	async fn encrypt_key(&self, key: &[u8]) -> Vec<u8> {
		if let Some(secret) = &self.secret {
			// Generate a random nonce for AES-GCM
			let nonce = OsRng.gen::<[u8; 12]>();

			// Encrypt the private key
			let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(
				keccak256(secret.expose_secret().as_bytes()).as_slice(),
			));
			let ciphertext = cipher.encrypt(Nonce::from_slice(&nonce), key.as_ref()).unwrap();

			// Combine nonce + ciphertext for storage
			let mut encrypted_key = Vec::with_capacity(nonce.len() + ciphertext.len());
			encrypted_key.extend_from_slice(&nonce);
			encrypted_key.extend_from_slice(&ciphertext);

			encrypted_key
		} else {
			key.to_vec()
		}
	}

	async fn decrypt_key(&self, key: &[u8]) -> Vec<u8> {
		if let Some(secret) = &self.secret {
			let nonce = &key[0..12];
			let ciphertext = &key[12..];

			// Decrypt the private key
			let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(
				keccak256(secret.expose_secret().as_bytes()).as_slice(),
			));
			cipher.decrypt(Nonce::from_slice(nonce), ciphertext.as_ref()).unwrap()
		} else {
			key.to_vec()
		}
	}

	async fn insert_key(&self, public_key: &[u8], private_key: &[u8]) {
		self.inner
			.db()
			.insert(ECDSA, &hex::encode(private_key), public_key)
			.expect(KEYSTORE_INTERNAL_ERROR)
	}

	#[cfg(test)]
	async fn test_get_key(
		&self,
		key_request: KeyRequest,
	) -> Result<Option<PrivateKey>, GetKeyError> {
		self.get_key(key_request, &Secp256k1::signing_only())
	}
}

impl GetKey for PasswordKeypairStorage {
	type Error = GetKeyError;

	fn get_key<C: Signing>(
		&self,
		key_request: KeyRequest,
		secp: &Secp256k1<C>,
	) -> Result<Option<PrivateKey>, Self::Error> {
		match key_request {
			KeyRequest::Pubkey(pk) => {
				match self.inner.db().raw_keystore_value::<AppPair>(
					&AppPublic::from_slice(pk.inner.serialize().as_slice())
						.expect(KEYSTORE_INTERNAL_ERROR),
				) {
					Ok(value) => {
						if let Some(value) = value {
							let decoded =
								hex::decode(value.as_bytes()).expect(KEYSTORE_INTERNAL_ERROR);
							let mut seed = if self.secret.is_some() {
								tokio::task::block_in_place(|| {
									tokio::runtime::Handle::current()
										.block_on(self.decrypt_key(&decoded))
								})
							} else {
								decoded
							};
							let private_key = PrivateKey::from_slice(&seed, self.inner.network)
								.expect(KEYSTORE_INTERNAL_ERROR);
							if private_key.public_key(secp) != pk {
								panic!("{}", KEYSTORE_DECRYPTION_ERROR);
							}
							seed.zeroize();
							Ok(Some(private_key))
						} else {
							Ok(None)
						}
					},
					Err(err) => {
						panic!("{:?}", err)
					},
				}
			},
			_ => Err(GetKeyError::NotSupported),
		}
	}
}
