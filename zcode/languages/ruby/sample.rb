#!/usr/bin/env ruby
# 見本: Ruby のクラス・シンボル・文字列の式展開・ヒアドキュメント
require "json"
require_relative "util"

=begin
ブロックコメント
=end

module Shapes
  VERSION = "1.0"
  MAX_ITEMS = 64

  class Shape
    attr_reader :name, :sides
    @@count = 0

    def initialize(name, sides: 3)
      @name = name
      @sides = sides
      @@count += 1
    end

    def self.count
      @@count
    end

    def area?
      false
    end

    def to_s
      "#{@name} has #{sides} sides (#{sides > 3 ? "many" : 'few'})\n"
    end

    def <=>(other)
      sides <=> other.sides
    end

    private

    def secret!
      $stderr.puts "hidden"
    end
  end

  class Circle < Shape
    PI = 3.14159

    def area(r = 1.5e0)
      PI * r**2
    end
  end
end

words = %w[alpha beta gamma]
opts = { color: :red, "size" => 10, nested: { ok: true } }
text = <<~EOS
  名前: #{opts[:color]}
  合計: #{words.map { |w| w.upcase }.join(", ")}
EOS

words.each_with_index do |w, i|
  next if w =~ /^a.*a$/i
  puts "#{i}: #{w}" unless w.empty?
end

case opts[:color]
when :red then puts text
when /gr(e|a)y/ then puts 'plain \' quote'
else raise ArgumentError, "bad: #{opts.inspect}"
end

total = (1..10).inject(0) { |s, n| s + n * 0x1F }
puts format("%05.2f", total / 3.0) if defined?(total)
