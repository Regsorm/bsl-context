#!/usr/bin/env python3
"""Группировка остаточных wrong_argument_count ПОСЛЕ фикса v0.3.2 (run_id>5).
Цель — подтвердить, что остаток это вариативные функции (СтрШаблон/Макс/Мин)."""
import os
import psycopg

DSN = os.environ.get("BSL_STAT_DSN",
                     "host=127.0.0.1 port=5432 dbname=rag user=claude_memory_rw")
METHOD_RE = r"Метод '([^']+)'"

with psycopg.connect(DSN) as c, c.cursor() as cur:
    cur.execute(
        """
        SELECT substring(message from %s) AS method,
               count(*) AS cnt,
               count(DISTINCT module_path) AS mods,
               min(message) AS sample
        FROM bsl_validation_findings
        WHERE kind = 'wrong_argument_count' AND run_id > 5
        GROUP BY 1 ORDER BY 2 DESC LIMIT 30
        """,
        (METHOD_RE,),
    )
    rows = cur.fetchall()
    print(f"{'МЕТОД':32} {'СРАБ':>7} {'МОД':>6}  ПРИМЕР СООБЩЕНИЯ")
    print("-" * 100)
    for m, cnt, mods, sample in rows:
        print(f"{(m or '?')[:32]:32} {cnt:>7} {mods:>6}  {sample[:55]}")
