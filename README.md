# ![Bifrost Network](media/bifrost_header.jpeg)

# Bifrost Relayer

The Bifrost Relayer is the core implementation that facilitates the Cross-Chain Communication Protocol (CCCP) for the
Bifrost Network. It processes cross-chain transactions and propagates data transfer (e.g. feeding price information)
from one blockchain to another.

## Getting Started

Learn to use the Bifrost network with our [technical docs](https://docs.bifrostnetwork.com/bifrost-network).

### Bifrost Network Testnet (ChainID: 49088)

| Public Endpoints (rpc/ws)                        |
| ------------------------------------------------ |
| https://public-01.testnet.bifrostnetwork.com/rpc |
| https://public-02.testnet.bifrostnetwork.com/rpc |
| wss://public-01.testnet.bifrostnetwork.com/wss   |
| wss://public-02.testnet.bifrostnetwork.com/wss   |

### Bifrost Network Mainnet (ChainID: 3068)

| Public Endpoints (rpc/ws)                        |
| ------------------------------------------------ |
| https://public-01.mainnet.bifrostnetwork.com/rpc |
| https://public-02.mainnet.bifrostnetwork.com/rpc |
| wss://public-01.mainnet.bifrostnetwork.com/wss   |
| wss://public-02.mainnet.bifrostnetwork.com/wss   |

### Install Requirements

To initiate the Bifrost Relayer, certain dependencies must be manually installed. Both the executable binary file and
the configuration YAML file are essential for all environments and operators.

First, install the latest Bifrost Relayer released binary.

```sh
wget "https://github.com/bifrost-platform/bifrost-relayer.rs/releases/latest/download/bifrost-relayer"
```

In order to execute the binary, the permission of the file has to be updated.

```sh
chmod +x bifrost-relayer
```

Then, install the configuration YAML file. This file serves as an example for a quick start. Given the minor differences
between Testnet and Mainnet environments, it's crucial to use the appropriate file for the corresponding network.

```sh
# For testnet only
wget "https://github.com/bifrost-platform/bifrost-relayer.rs/releases/latest/download/config.testnet.yaml"

# For mainnet only
wget "https://github.com/bifrost-platform/bifrost-relayer.rs/releases/latest/download/config.mainnet.yaml"
```

### Configuration Setup

Next, the configuration YAML file contains certain parameters that the operator has to set. For instance, variables such
as the relayer private key and each EVM provider's RPC endpoints depend on the operator, thus these values should be
manuall input.

You should prepare RPC endpoints for the following blockchain networks. There are two options for this: 1) operating your own blockchains nodes, or 2) utilizing services that offers RPC endpoints. You can find node providers on the links below. **It’s crucial that each node must be archive-mode enabled**.

- [Bifrost](https://docs.bifrostnetwork.com/bifrost-network/running-a-node/guide-for-operators/setting-up-a-validator-node) (**Must be priorly self-operating and fully synced**)
- [Ethereum](https://ethereum.org/en/developers/docs/nodes-and-clients/nodes-as-a-service/#popular-node-services)
- [Binance Smart Chain](https://docs.bnbchain.org/docs/rpc)
- [Polygon](https://wiki.polygon.technology/docs/pos/reference/rpc-endpoints/)
- [Base](https://docs.base.org/tools/node-providers/)
- [Arbitrum](https://docs.arbitrum.io/node-running/node-providers)

You should also prepare an EVM account that will act as your relayer account. This account should have enough balance
for transaction fees used in operations.

### Run the Relayer

Use the following command to execute the Bifrost Relayer. The `<PATH_TO_CONFIG_FILE>` should be set to the absolute path
of the installed configuration YAML file.

```sh
bifrost-relayer --chain <PATH_TO_CONFIG_FILE>
```

## Development

To build and develop Bifrost Relayer, you will need a proper development environment. If you've never worked with a
Rust-based project before, your should probably try to first install `rustup`.

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Then, fetch the project's code by using git.

```sh
git clone https://github.com/bifrost-platform/bifrost-relayer.rs
cd bifrost-relayer.rs
```

And now build the project to generate the executable binary file. (The first build will take some time to end)

```sh
cargo build --release
```

You can now run your relayer after if you have finished configuration setup. The configuration YAML files exists in
the `configs/` directory. Execute the relayer using the following command.

```sh
# For testnet only
./target/release/bifrost-relayer --chain testnet

# For mainnet only
./target/release/bifrost-relayer --chain mainnet
```
