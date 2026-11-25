# Standalone original stock ngspice47-only d_source + d_to_real C-model bug
# evidence. Source input 0s,Us,1s,Us changes each nanosecond; original C
# schedules the real output +180ps. Verify the entire authored trace.
function abs(v){return v<0?-v:v}
function die(s){print s >"/dev/stderr";bad=1}
function typ(s){
    if(tolower(s) ~ /nan/)return "nan"
    if(tolower(s)=="inf" || tolower(s)=="+inf")return "inf"
    if(s+0==0)return "zero"
    if((s+0)>9.9e307 && (s+0)<1.01e308)return "finite"
    return "other"
}
BEGIN{
 split("0 1 1.18 2 2.18 3 3.18",time," ")
 split("0s Us Us 1s 1s Us Us",source," ")
 split("finite finite inf inf finite finite inf",enabled," ")
 split("zero zero nan nan zero zero nan",disabled," ")
}
$1~/^[0-9]/ {
 ++n
 if(n>7 || NF!=(DISABLED?4:3)){
   die(sprintf("wrong original standalone eprint number/shape at %s:%d",FILENAME,FNR));exit 1
 }
 if(abs($1*1e9-time[n])>.000001 || $2!=source[n] ||
    DISABLED && $3!="0s" ||
    typ($(DISABLED?4:3))!=(DISABLED?disabled[n]:enabled[n])){
   die(sprintf("original source standalone midpoint %s wrong row %d time %.7g state %s real %s",
       DISABLED?"disabled":"enabled",n,$1*1e9,$2,$(DISABLED?4:3)))
   exit 1
 }
}
END{
 if(bad)exit 1
 if(n!=7){die(sprintf("original stock-only C-model midpoint output incomplete: %d/7",n));exit 1}
 printf("Stock ngspice47 built-in original d_to_real standalone midpoint: 7/7 real event observations, %s UNKNOWN --> %s PASS\n",
        DISABLED?"disabled":"enabled",DISABLED?"NaN":"Inf")
 exit bad
}
