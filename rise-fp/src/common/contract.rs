use alloy_sol_macro::sol;
use serde::{Deserialize, Serialize};

sol! {
    type GameType is uint32;
    type Claim is bytes32;
    type Timestamp is uint64;
    type Hash is bytes32;

    #[sol(rpc)]
    #[derive(Debug)]
    contract DisputeGameFactory {
        event DisputeGameCreated(address indexed disputeProxy, GameType indexed gameType, Claim indexed rootClaim);
        event ImplementationSet(address indexed impl, GameType indexed gameType);

        mapping(GameType => IDisputeGame) public gameImpls;
        mapping(GameType => uint256) public initBonds;

        function gameCount() external view returns (uint256 gameCount_);
        function gameAtIndex(uint256 _index) external view returns (GameType gameType_, Timestamp timestamp_, IDisputeGame proxy_);
        function games(GameType gameType, Claim rootClaim, bytes extraData) external view returns (IDisputeGame proxy_, Timestamp timestamp_);
        function getGameUUID(GameType gameType, Claim rootClaim, bytes extraData) external pure returns (Hash uuid_);
        function create(GameType _gameType, Claim _rootClaim, bytes calldata _extraData) external payable returns (IDisputeGame proxy_);
    }

    #[allow(missing_docs)]
    #[sol(rpc)]
    interface IDisputeGame {
        function status() external view returns (GameStatus status_);
    }

    #[allow(missing_docs)]
    #[sol(rpc)]
    interface IFaultDisputeGame {
        function l2SequenceNumber() external view returns (uint256 l2SequenceNumber_);
    }

    #[sol(rpc)]
    contract OPSuccinctFaultDisputeGame {
        function gameType() public pure returns (GameType gameType_);
        function gameCreator() public pure returns (address creator_);
        function l2SequenceNumber() public pure returns (uint256 l2SequenceNumber_);
        function l2BlockNumber() public pure returns (uint256 l2BlockNumber_);
        function parentIndex() public pure returns (uint32 parentIndex_);
        function startingBlockNumber() external view returns (uint256 startingBlockNumber_);
        function rootClaim() public pure returns (Claim rootClaim_);
        function l1Head() public pure returns (Hash l1Head_);
        function status() public view returns (GameStatus status_);
        function claimData() public view returns (ClaimData memory claimData_);
        function wasRespectedGameTypeWhenCreated() external view returns (bool wasRespectedGameTypeWhenCreated_);
        function challenge() external payable returns (ProposalStatus);
        function prove(bytes calldata proofBytes) external returns (ProposalStatus);
        function resolve() external returns (GameStatus status_);
        function gameOver() external view returns (bool gameOver_);
        function maxChallengeDuration() external view returns (uint256 maxChallengeDuration_);
        function maxProveDuration() external view returns (uint64 maxProveDuration_);
        function anchorStateRegistry() external view returns (address registry_);
        function challengerBond() external view returns (uint256 challengerBond_);
        function aggregationVkey() external view returns (bytes32 aggregationVkey_);
        function rangeVkeyCommitment() external view returns (bytes32 rangeVkeyCommitment_);
        function rollupConfigHash() external view returns (bytes32 rollupConfigHash_);
        function claimCredit(address _recipient) external;
        function credit(address _recipient) external view returns (uint256 credit_);
    }

    #[allow(missing_docs)]
    #[sol(rpc)]
    contract AnchorStateRegistry {
        function getAnchorRoot() public view returns (Hash, uint256);
        function isGameBlacklisted(IDisputeGame _game) public view returns (bool);
        function isGameFinalized(IDisputeGame _game) public view returns (bool);
        function anchorGame() public view returns (IDisputeGame anchorGame_);
        function respectedGameType() external view returns (GameType);
    }

    #[sol(rpc)]
    contract SystemConfig {
        function optimismPortal() external view returns (OptimismPortal);
    }

    #[sol(rpc)]
    contract OptimismPortal {
        function anchorStateRegistry() external view returns (AnchorStateRegistry);
        function disputeGameFactory() external view returns (DisputeGameFactory);
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "SCREAMING_SNAKE_CASE")]
    /// @notice The current status of the dispute game.
    enum GameStatus {
        IN_PROGRESS,
        CHALLENGER_WINS,
        DEFENDER_WINS
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    enum ProposalStatus {
        Unchallenged,
        Challenged,
        UnchallengedAndValidProofProvided,
        ChallengedAndValidProofProvided,
        Resolved
    }

    /// @notice The `ClaimData` struct represents the data associated with a Claim.
    #[derive(Debug)]
    struct ClaimData {
        uint32 parentIndex;
        address counteredBy;
        address prover;
        Claim claim;
        ProposalStatus status;
        Timestamp deadline;
    }

    /// @notice The `L2Output` struct represents the L2 output.
    struct L2Output {
        uint64 zero;
        bytes32 l2_state_root;
        bytes32 l2_storage_hash;
        bytes32 l2_claim_hash;
    }
}
