# frozen_string_literal: true

lib = File.expand_path("lib", __dir__)
$LOAD_PATH.unshift(lib) unless $LOAD_PATH.include?(lib)
require "relay/version"

Gem::Specification.new do |s|
  s.name        = "relay"
  s.version     = Relay::VERSION
  s.summary     = "Ruby SDK for Railyard Relay inference and AITP payments"
  s.description = "A dependency-free Ruby SDK for Relay's inference, streaming, AITP paid " \
                  "retry, catalog, quote, settlement, and cost-event HTTP surfaces."
  s.authors     = ["Railyard"]
  s.license     = "Apache-2.0"

  s.required_ruby_version = ">= 2.6"

  s.files = Dir[
    "lib/**/*.rb",
    "README.md",
    "relay.gemspec"
  ]
  s.require_paths = ["lib"]

  s.add_development_dependency "minitest", "~> 5.0"
  s.add_development_dependency "rake", "~> 13.0"
end
