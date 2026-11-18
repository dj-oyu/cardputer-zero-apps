"""数を集める小さなクラス。"""
from dataclasses import dataclass, field


@dataclass
class Stats:
    values: list[int] = field(default_factory=list)

    @property
    def total(self) -> int:
        return sum(self.values)

    def add(self, v: int) -> None:
        self.values.append(v)

    def mean(self) -> float:
        # 空なら 0（ゼロ除算を避ける）
        return self.total / len(self.values) if self.values else 0.0
