## migrate-keystore

Use the `bifrost-relayer migrate-keystore` command to migrate the current keystore to a new AWS-KMS or password based keystore.

#### Scenarios

1. Migrate from password based keystore to AWS-KMS based keystore.
2. Migrate from AWS-KMS based keystore to password based keystore.
3. Migrate from password based keystore to password based keystore.
4. Migrate from AWS-KMS based keystore to AWS-KMS based keystore.
5. Migrate from password(v1) based keystore to password(v2) based keystore. (This will be removed in the future)

#### Basic command usage

```shell
bifrost-relayer migrate-keystore [options]
```

#### Options

You can use the following command-line options with the `bifrost-relayer migrate-keystore` command.

| Option   | Description
| -------- | -----------
| `--round <index>` | Specifies the RegistrationPool round index to navigate which keystore directory to migrate.
| `--chain <chain-specification>` | Specifies the chain specification to use. You can set this option using a predefined chain specification name, such as `mainnet` or `testnet`, or you can specify the path to a file that contains the chain specification. The previous keystore credentials must be provided in the configuration file.
| `--new-kms-key-id <key-id>` | Specifies the new KMS key ID to use for the keystore. This option has higher priority than `--new-password`.
| `--new-password <password>` | Specifies the new password to use for the keystore.
| `--version <version>` | Specifies the keystore version to migrate. The default is `2`. This option is only used for password based keystore when migrating from v1 to v2. (This will be removed in the future)

#### Example

1. Migrating to AWS-KMS based keystore.
```shell
bifrost-relayer migrate-keystore --round 1 --chain mainnet --new-kms-key-id "mynewkmsid"
```

2. Migrating to password based keystore.
```shell
bifrost-relayer migrate-keystore --round 1 --chain mainnet --new-password "mynewpassword"
```
