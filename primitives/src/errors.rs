pub const INVALID_CONTRACT_ABI: &str =
	"Invalid contract ABI provided. Please check your contract's ABI.";

pub const INVALID_CONTRACT_ADDRESS: &str =
	"Invalid contract address provided. Please check your contract's address.";

pub const INVALID_PRIVATE_KEY: &str =
	"Invalid private key provided. Please check your relayer's private key.";

pub const INVALID_PROVIDER_URL: &str =
	"Invalid provider URL provided. Please check your provider's URL.";

pub const INVALID_CHAIN_ID: &str =
	"Invalid chain ID provided. Please check your provider's or contract's chain ID.";

pub const INVALID_BIFROST_NATIVENESS: &str =
	"BIFROST Network is not initialized as native. Please check your provider's `is_native` field.";

pub const INVALID_PERIODIC_SCHEDULE: &str =
	"Invalid periodic schedule format provided. Please check your schedule format.";

pub const INVALID_CONFIG_FILE_PATH: &str =
	"Invalid config.yaml file path provided. Please check your file path.";

pub const INVALID_CONFIG_FILE_STRUCTURE: &str =
	"Invalid config.yaml file structure provided. Please check your file structure.";

pub const INVALID_CHAIN_SPECIFICATION: &str =
	"Invalid --chain specification provided. Please check your CLI options.";

pub const INSUFFICIENT_FUNDS: &str =
	"Insufficient funds. Please check your relayer's remaining balance.";

pub const NETWORK_DOES_NOT_SUPPORT_EIP1559: &str =
	"Network does not support EIP-1559 transaction. Please check your evm_providers config";

pub const PROVIDER_INTERNAL_ERROR: &str =
	"An internal error thrown when making a call to the provider. Please check your provider's status";

pub const PARAMETER_OUT_OF_RANGE: &str =
	"An invalid parameter is out of range. Please check your configuration file.";
