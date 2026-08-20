// Caminho de arquivo: resolve + leitura, o que fica quente quando o script
// para de usar shell pra ler.
let total = 0;
for i in 0..300 { total += read_file("bench/casos/trivial.rhai").len(); }
let achados = glob("bench/casos/*.rhai").len();
