# Plaza: A Foundation for Real-Time Shared State

**License:** Mozilla Public License 2.0 (MPL-2.0)
**Status:** Experimental

Plaza is a server-side foundation for building real-time, multi-user applications where shared state is central. It provides a structured approach to managing concurrent interactions and state synchronization.

## Core Idea

Plaza revolves around a **server-authoritative state model** where all changes are driven by discrete **operations (Ops)**. This ensures consistency and provides a clear history of state evolution. An application defines its own:

1.  **Shared State (`StateType`):** The data your application manages.
2.  **Operations (`Op`):** The actions that can modify the state.
3.  **Logic (`StateLogic`):** The rules dictating how Ops transform the StateType.

A central **Controller** orchestrates these components, processing Ops from connected users (via a **Session** abstraction) and managing **Snapshots** for efficient state synchronization.

## Philosophy

*   **Foundational & Unopinionated:** Plaza offers core building blocks, not a restrictive framework. You build *with* Plaza, not *in* Plaza.
*   **Decoupled by Design:** Encourages separation of state, logic, and networking.
*   **Focus on Server-Side:** Provides tools for the backend of your real-time application.

## Potential Building Blocks

Beyond its core, the Plaza ecosystem aims to explore and provide optional, reusable components and patterns, such as:

*   **Time Management:** Schedulers for timed events and game ticks.
*   **State Organization:** Finite State Machines (FSMs) for managing complex states.
*   **Multi-Instance Support (e.g., Lobbies/Rooms):** Standardized data payloads and example architectures for managing multiple concurrent game sessions or collaborative workspaces on a single server.
*   **Networking Patterns:** Server-side utilities to support client-side prediction, reconciliation, and lag compensation techniques.
*   **Reusable Session Adapters:** Implementations for common network transports (e.g., WebSockets).

## Target Applications

Plaza is being explored for applications like:

*   Multiplayer games (turn-based, simple real-time).
*   Real-time collaborative tools (shared editors, whiteboards).
*   Any system requiring synchronized shared state among multiple users.

## Current Status & Contributing

Plaza is currently **experimental**. Its APIs and internal architecture are subject to change. We welcome discussion, feedback, and contributions as we explore its capabilities and refine its design. The primary implementation is in Rust, but the core concepts are language-agnostic.