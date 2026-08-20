// desc: roda os casos de bench/casos e compara com bench/baseline.json.
// uso:  codemode run bench-all.rhai [--arg salvar]
let salvar = ARGS.len() > 0 && ARGS[0] == "salvar";
let casos = glob("bench/casos/*.rhai");
let linhas = [];
let novo = "{\n";
let primeiro = true;
for caso in casos {
    let r = run_shell_full("./target/release/codemode bench " + caso + " --workdir . --n 15");
    let saida = r.stdout;
    // "codemode     median=  4.123ms  min=..."
    let i = saida.index_of("median=");
    let mediana = "?";
    if i >= 0 {
        let resto = saida.sub_string(i + 7);
        let j = resto.index_of("ms");
        if j >= 0 {
            mediana = resto.sub_string(0, j);
            mediana.trim();
        }
    }
    let nome = basename(caso);
    linhas.push(nome + "\t" + mediana);
    if !primeiro { novo += ",\n"; }
    novo += "  \"" + nome + "\": " + mediana;
    primeiro = false;
}
novo += "\n}\n";

let base = "{}";
if path_exists("bench/baseline.json") { base = read_file("bench/baseline.json"); }
let referencia = from_json(base);

print("caso                    mediana      referencia   delta");
print("---------------------------------------------------------");
for l in linhas {
    let partes = l.split("\t");
    let nome = partes[0];
    let atual = parse_float(partes[1]);
    let ref_txt = "--";
    let delta = "--";
    if referencia.contains(nome) {
        let r = referencia[nome];
        ref_txt = "" + r + "ms";
        let d = ((atual - r) / r) * 100.0;
        delta = "" + (d * 10.0).round() / 10.0 + "%";
    }
    print(nome + "   " + atual + "ms   " + ref_txt + "   " + delta);
}

if salvar {
    write_file_force("bench/baseline.json", novo);
    print("");
    print("baseline regravada: bench/baseline.json");
}
