// 型の誤り（実機の tsc）
function len(s: string): number {
  return s.length;
}
console.log(len(42));
