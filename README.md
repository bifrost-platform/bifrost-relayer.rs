# ![BIFROST Network](media/bifrost_header.jpeg)

### Designed to be the successor of https://github.com/bifrost-platform/bifrost-relayer.

The BIFROST Network is a fast and scalable EVM-compatible blockchain that
provides an all-in-one environment for developers to build multichain DApps.

## Getting Started

Learn to use the BIFROST network with our [technical docs](https://docs.thebifrost.io/bifrost-network).

### BIFROST Network Testnet (ChainID: 49088)
|Public Endpoints (rpc/ws)|
|------|
|https://public-01.testnet.thebifrost.io/rpc|
|https://public-02.testnet.thebifrost.io/rpc|
|wss://public-01.testnet.thebifrost.io/ws|
|wss://public-02.testnet.thebifrost.io/ws|

### BIFROST Network Mainnet (ChainID: 3068)
|Public Endpoints (rpc/ws)|
|------|
|https://public-01.mainnet.thebifrost.io/rpc|
|https://public-02.mainnet.thebifrost.io/rpc|
|wss://public-01.mainnet.thebifrost.io/wss|
|wss://public-02.mainnet.thebifrost.io/wss|

### Requirements

The following items are the essential requirements that must be prepared to be able to launch the relayer.

**1. RPC Endpoints for each supporting network. The node must be archive-mode enabled.**

**2. An EVM account for cross-chain action participation. It should have enough balance for transaction fees used in operations.**


### Build

Use the following command to build the relayer
without launching it:

```sh
cargo build --release
```

We can also use docker to build the relayer. Follow the command below:

```sh
TODO
```

### Run Relayer

Use the following command to launch the relayer:

```sh
./target/release/cccp-relayer
```

We can also use docker to launch the relayer. Follow the command below:

```sh
TODO
```

## Project Structure

TODO
