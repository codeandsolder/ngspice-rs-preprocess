# Independently authored exact digital state+strength plateaus for the
# original d_genlut -> d_and/d_xor fanout. Unlike the physical DAC,
# eprint distinguishes Uz (high impedance) from Uu (undetermined).
function fail(s){print s > "/dev/stderr";bad=1}
function check(i, bits, j, expected){
    bits=((i-1)%4==0?"00Z0X":(i-1)%4==1?"10001":(i-1)%4==2?"01Z0X":"10110")
    for(j=1;j<=5;j++){
        expected=substr(bits,j,1)
        expected=expected=="Z"?"Uz":expected=="X"?"Us":expected "s"
        if(previous[j]!=expected)
            fail(sprintf("%s %.3f ns channel %d expected %s got %s",FILENAME,sample[i],j,expected,previous[j]))
    }
}
BEGIN{n=split("5 15 25 35 45 55 65 75 85 95 105 115 123",sample," ");i=1}
$1~/^[0-9]/{
    if(NF!=6){fail("malformed original genlut event row");exit 1}
    t=$1*1e9
    if(rows&&t<last){fail("nonmonotonic event trace");exit 1}
    while(i<=n&&sample[i]<t){
        if(!rows){fail("missing event initialization");exit 1}
        check(i++)
    }
    for(j=1;j<=5;j++)previous[j]=$(j+1)
    rows++;last=t
}
END{
    if(!rows)fail("missing original genlut events")
    while(rows&&i<=n)check(i++)
    if(bad)exit 1
    printf "Original d_genlut event strength: %d plateaus x 5 signals PASS (%s)\n",n,FILENAME
}
