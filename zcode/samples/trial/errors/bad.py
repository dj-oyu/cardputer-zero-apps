# 型の誤り（pyright）。4 行目と 7 行目に波線が出る。
def double(x: int) -> int:
    return x * 2
label: str = double(3)


print(undefined_name)
