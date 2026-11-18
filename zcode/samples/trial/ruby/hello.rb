# Ruby のサンプル（F5 は実機の ruby。保存でビルド係が ruby -wc）。
Item = Struct.new(:name, :count)

items = [Item.new("ねじ", 12), Item.new("ナット", 7), Item.new("ばね", 3)]
items.sort_by(&:count).each do |it|
  puts "#{it.name}: #{"#" * it.count}"
end
puts "合計 #{items.sum(&:count)} 個"
