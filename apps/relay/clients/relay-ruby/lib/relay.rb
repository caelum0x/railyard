# frozen_string_literal: true

require_relative "relay/version"
require_relative "relay/errors"
require_relative "relay/aitp"
require_relative "relay/transport"
require_relative "relay/multipart"
require_relative "relay/client"

# Relay — native Ruby client for the Relay gateway (a TensorZero fork).
#
#   require "relay"
#   client = Relay::Client.new(base_url: "http://localhost:3000", api_key: ENV["RELAY_API_KEY"])
#   client.chat_completions(model: Relay.model_target("gpt-4o-mini"),
#                           messages: [{ role: "user", content: "hi" }])
module Relay
  # Build the `model` field targeting a configured Relay model.
  # @return [String] e.g. "tensorzero::model_name::gpt-4o-mini"
  def self.model_target(name)
    "tensorzero::model_name::#{name}"
  end

  # Build the `model` field targeting a configured Relay function.
  # @return [String] e.g. "tensorzero::function_name::summarize"
  def self.function_target(name)
    "tensorzero::function_name::#{name}"
  end
end
