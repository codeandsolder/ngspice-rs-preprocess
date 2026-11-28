BEGIN {
    FS="[ ,\t]+";
    tolerance_ns=0.12;
}
FNR==NR {
    if ($1==name && $2!="transition") {
        reference[++expected]=$3+0;
        split($2,parts,"-");
        directions[expected]=parts[2];
    }
    next;
}
NF>=2 {
    time=($1=="" ? $2 : $1)+0;
    voltage=($1=="" ? $3 : $2)+0;
    if (have_prev && (last_voltage-2.5)*(voltage-2.5)<0) {
        crossing=(last_time+(time-last_time)*(2.5-last_voltage)/(voltage-last_voltage))*1e9;
        dir=(voltage>last_voltage ? "rise":"fall");
        observed++;
        if (observed>expected) {
            printf "unexpected native transition %s %.9f ns for %s\n",dir,crossing,name >"/dev/stderr";
            bad=1;
        } else {
            delta=crossing-reference[observed];
            absolute=(delta>=0 ? delta:-delta);
            printf "%s #%d: vendor %.9f ns, native %.9f ns, delta %+.3f ps\n",name,observed,reference[observed],crossing,delta*1000;
            if (dir != directions[observed] || absolute>tolerance_ns) {
                printf "native/vendor event mismatch for %s event %d\n",name,observed >"/dev/stderr";
                bad=1;
            }
        }
    }
    have_prev=1;
    last_time=time;
    last_voltage=voltage;
}
END {
    if (expected==0 || observed!=expected) {
        printf "event count mismatch for %s: expected %d observed %d\n",name,expected,observed >"/dev/stderr";
        bad=1;
    }
    exit bad;
}
