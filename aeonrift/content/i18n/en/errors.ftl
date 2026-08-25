# Aeonrift — Error / status message localization (en)
#
# User-facing error and status strings for client and services.
# Keep messages actionable; never leak internal details.

## Connectivity

error-network-lost = Connection lost. Trying to reconnect…
error-network-timeout = The server did not respond. Please check your connection.
error-server-unavailable = Servers are unavailable right now. Please try again later.
error-reconnecting = Reconnecting… ({ $attempt } of { $max })
error-reconnect-failed = Could not reconnect to the match.

## Authentication

error-auth-required = You must be signed in to continue.
error-auth-invalid = Incorrect username or password.
error-auth-expired = Your session has expired. Please sign in again.
error-auth-banned = This account is suspended.

## Matchmaking

error-match-declined = Match cancelled: a player declined.
error-match-timeout = Match cancelled: a player did not respond.
error-match-abandoned = You left a match and may receive a penalty.
error-queue-dodge = A player dodged. Returning to the lobby.

## Champion select / gameplay

error-champion-locked = That champion is not available.
error-champion-banned = That champion has been banned this game.
error-champion-already-picked = That champion is already taken.
error-invalid-target = Invalid target.
error-out-of-range = Target is out of range.
error-not-enough-mana = Not enough mana.
error-not-enough-energy = Not enough energy.
error-ability-on-cooldown = Ability on cooldown ({ $seconds }s).

## Shop / economy

error-not-enough-gold = Not enough gold.
error-item-not-available = That item is not available.
error-inventory-full = Your inventory is full.

## Content / loading

error-content-load-failed = Failed to load game content. Please restart the client.
error-content-corrupt = Game files appear to be corrupted. A repair is recommended.
error-version-mismatch = Your client is out of date. Please update to continue.

## Generic

error-unknown = Something went wrong. Please try again.
error-retry = Retry
error-report = Report a Problem
error-details = Error code: { $code }
