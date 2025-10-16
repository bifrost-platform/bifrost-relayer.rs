use super::*;
use miniscript::bitcoin::psbt::SigningKeysMap;

#[derive(Clone)]
/// A keystore for KMS-based keypairs.
pub struct KmsKeypairStorage {
	/// The keystore container.
	pub inner: KeystoreContainer,
	/// The KMS key ID for encryption/decryption.
	pub key_id: String,
	/// The KMS client.
	pub client: Arc<KmsClient>,
}

impl KmsKeypairStorage {
	pub fn new(
		path: String,
		network: Network,
		key_id: String,
		client: Arc<KmsClient>,
	) -> Arc<RwLock<Self>> {
		Arc::new(RwLock::new(Self { inner: KeystoreContainer::new(path, network), key_id, client }))
	}
}

#[async_trait::async_trait]
impl KeypairStorageT for KmsKeypairStorage {
	async fn load(&mut self, round: u32) {
		self.inner.load(round);
	}

	async fn load_v1(&mut self, _: u32, _: Option<SecretString>) {
		panic!("KMS keystore is not supported for version 1")
	}

	async fn db(&self) -> Arc<LocalKeystore> {
		self.inner.db()
	}

	async fn sign_psbt_inner(
		&self,
		psbt: &mut Psbt,
	) -> Result<SigningKeysMap, (SigningKeysMap, SigningErrors)> {
		Ok(psbt.sign(self, &Secp256k1::new())?)
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
		let encrypted_key = self
			.client
			.encrypt()
			.key_id(self.key_id.clone())
			.plaintext(Blob::new(key))
			.send()
			.await
			.expect("Failed to encrypt");
		encrypted_key.ciphertext_blob.expect("Failed to encrypt").as_ref().to_vec()
	}

	async fn decrypt_key(&self, key: &[u8]) -> Vec<u8> {
		let decrypt_result = self
			.client
			.decrypt()
			.key_id(self.key_id.clone())
			.ciphertext_blob(Blob::new(key))
			.send()
			.await
			.expect("Failed to decrypt");

		decrypt_result.plaintext.expect("Failed to decrypt").into_inner()
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

impl GetKey for KmsKeypairStorage {
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
							let mut seed = tokio::task::block_in_place(|| {
								tokio::runtime::Handle::current()
									.block_on(self.decrypt_key(&decoded))
							});
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
