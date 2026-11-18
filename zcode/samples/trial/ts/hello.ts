// TypeScript のサンプル（保存で実機の tsc が型を見る。F5 は tsc → LLRT）。
interface Reading {
  sensor: string;
  celsius: number;
}

function average(rs: Reading[]): number {
  return rs.reduce((s, r) => s + r.celsius, 0) / rs.length;
}

const readings: Reading[] = [
  { sensor: "CPU", celsius: 56.2 },
  { sensor: "電池", celsius: 31.5 },
];
console.log(`平均 ${average(readings).toFixed(1)}℃`);
