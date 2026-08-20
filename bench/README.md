# bench/

Casos versionados de performance. Existem porque toda medição do roadmap v1.0
foi feita à mão em scripts de `/tmp` que se perderam — e sem caso versionado a
próxima mudança de performance vira discussão de opinião (issue #35).

```
codemode run bench-all.rhai            # roda os casos e imprime a tabela
codemode run bench-all.rhai --arg salvar   # regrava bench/baseline.json
```

`baseline.json` guarda a mediana de referência de cada caso **desta máquina**.
Número de outra máquina não é comparável: o que se compara é a razão
antes/depois no mesmo lugar. O job de CI roda os casos e imprime o resultado,
mas **não falha** por regressão: runner compartilhado varia demais para isso
ser sinal, e um teste que dá alarme falso é pior que teste nenhum.
