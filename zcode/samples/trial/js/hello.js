// JavaScript のサンプル（F5 は実機の LLRT）。
const words = ["zcode", "は", "小さな", "エディタ"];
const lens = words.map((w) => [...w].length);
console.log(words.join(" "));
console.log("文字数:", lens.join(", "), "合計", lens.reduce((a, b) => a + b, 0));
